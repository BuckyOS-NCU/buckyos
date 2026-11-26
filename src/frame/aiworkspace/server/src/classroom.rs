use crate::{blocking, pages, s, AppState};
use aiworkspace_core::{Code, Deny, WsError, WsResult};
use aiworkspace_store::classroom::ClassroomKind;
use aiworkspace_store::entries::split_entry_id;
use aiworkspace_store::Caller;
use serde_json::{json, Value};
use std::sync::Arc;

fn unavailable() -> WsError {
    WsError::refused(
        Code::NotFound,
        Deny::EntryUnavailable,
        "classroom access is unavailable",
    )
}

async fn require_active(state: &AppState, principal: &str) -> WsResult<()> {
    match state.auth.classroom_account_active(principal).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(unavailable()),
        Err(_) => Err(WsError::refused(
            Code::PermissionDenied,
            Deny::AuthorizationUnavailable,
            "could not verify classroom account",
        )),
    }
}

pub(super) async fn handle(state: &Arc<AppState>, method: &str, params: &Value, caller: &Caller) -> WsResult<Value> {
    let workspace = s(params, "workspace_id")?.to_owned();
    if method == "ws.classroom_request" {
        if !caller.is_direct_workspace_account() {
            return Err(unavailable());
        }
        require_active(state, &caller.principal).await?;
        let surface = s(params, "surface_id")?.to_owned();
        let (st, c) = (state.clone(), caller.clone());
        return blocking(move || st.with_ws(&workspace, |ws| ws.classroom_request(&c, &surface))).await;
    }
    let (st, w, c) = (state.clone(), workspace.clone(), caller.clone());
    blocking(move || st.with_ws(&w, |ws| ws.require_classroom_owner(&c))).await?;
    let (st, c) = (state.clone(), caller.clone());
    match method {
        "ws.classroom_requests" => blocking(move || st.with_ws(&workspace, |ws| ws.classroom_requests(&c))).await,
        "ws.classroom_issue" => {
            let recipient = s(params, "recipient")?.to_owned();
            let scope = s(params, "scope_id")?.to_owned();
            let kind: ClassroomKind = serde_json::from_value(params.get("kind").cloned().unwrap_or_default())
                .map_err(|_| WsError::invalid_op("classroom kind must be guest or student"))?;
            require_active(state, &recipient).await?;
            blocking(move || st.with_ws(&workspace, |ws| ws.classroom_issue(&c, &recipient, &scope, kind))).await
        }
        "ws.classroom_approve" => {
            let request = s(params, "request_id")?.to_owned();
            let (check_state, w, check_caller, request_id) =
                (st.clone(), workspace.clone(), c.clone(), request.clone());
            let recipient = blocking(move || {
                check_state.with_ws(&w, |ws| ws.classroom_request_recipient(&check_caller, &request_id))
            })
            .await?;
            require_active(state, &recipient).await?;
            blocking(move || st.with_ws(&workspace, |ws| ws.classroom_approve(&c, &request, &recipient))).await
        }
        "ws.classroom_revoke" => {
            let (wid, local) = split_entry_id(s(params, "entry_id")?).ok_or_else(unavailable)?;
            if wid != workspace {
                return Err(unavailable());
            }
            let entry = local.to_owned();
            blocking(move || st.with_ws(&workspace, |ws| ws.classroom_revoke(&c, &entry))).await
        }
        _ => Err(WsError::new(Code::InvalidOperation, crate::UNKNOWN_METHOD)),
    }
}

pub(super) async fn redeem(state: &Arc<AppState>, workspace: &str, local: &str, caller: &Caller) -> WsResult<Value> {
    if !caller.is_direct_workspace_account() {
        return Err(unavailable());
    }
    require_active(state, &caller.principal).await?;
    let (st, w, l, c) = (state.clone(), workspace.to_owned(), local.to_owned(), caller.clone());
    let (scope, title, slug) = blocking(move || {
        st.with_ws(&w, |ws| {
            let (scope, title) = ws.classroom_redeem(&c, &l)?;
            Ok((scope, title, ws.workspace_slug()))
        })
    })
    .await?;
    let path = pages::page_url(
        state,
        workspace,
        &slug,
        title.as_deref().map(|title| (title, scope.as_str())),
    )
    .unwrap_or_else(|| "/".to_string());
    Ok(json!({"ok":true,"workspace_id":workspace,"joined":true,"path":path}))
}

pub(super) async fn download(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path((workspace_id, asset_entity_id)): axum::extract::Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use axum::{http::header, response::IntoResponse};

    const ROUTE: &str = "classroom.download_asset";
    let caller = match crate::bearer(&state, &headers, ROUTE, Some(&workspace_id)).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let actor = caller.clone();
    let result =
        blocking(move || state.with_ws(&workspace_id, |ws| ws.download_approval_asset(&actor, &asset_entity_id))).await;
    match result {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/octet-stream"),
                (header::CONTENT_DISPOSITION, "attachment"),
                (header::CACHE_CONTROL, "private, no-store"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            bytes,
        )
            .into_response(),
        Err(error) => crate::http_error(error, Some(&caller), ROUTE),
    }
}
