import { useCallback, useEffect, useState } from 'react'
import { unwrap, type AiwsClient, type ClassroomAccessKind, type ClassroomAccessRequest, type ClassroomInvitation } from '../../api/client'
import { entryUrl } from '../../links'
import { useI18n } from '../../../../i18n/provider'

export function ClassroomShare({ client, workspaceId, surfaceId }: {
  client: AiwsClient
  workspaceId: string
  surfaceId: string | null
}) {
  const { t } = useI18n()
  const [requests, setRequests] = useState<ClassroomAccessRequest[]>([])
  const [invitations, setInvitations] = useState<ClassroomInvitation[]>([])
  const [recipient, setRecipient] = useState('')
  const [kind, setKind] = useState<ClassroomAccessKind>('guest')
  const [entry, setEntry] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [failed, setFailed] = useState(false)

  const load = useCallback(async () => unwrap(await client.classroomRequests({ workspace_id: workspaceId })), [client, workspaceId])
  useEffect(() => {
    let active = true
    void load().then((value) => { if (active) { setRequests(value.requests); setInvitations(value.invitations) } }).catch(() => { if (active) setFailed(true) })
    return () => { active = false }
  }, [load])

  const refresh = async () => {
    const value = await load()
    setRequests(value.requests)
    setInvitations(value.invitations)
  }
  const perform = async (action: () => Promise<void>) => {
    setBusy(true)
    setFailed(false)
    try { await action() } catch { setFailed(true) }
    finally { setBusy(false) }
  }
  const issue = () => void perform(async () => {
    if (!surfaceId) return
    const issued = unwrap(await client.classroomIssue({ workspace_id: workspaceId }, recipient.trim(), surfaceId, kind))
    setEntry(issued.entry_id)
    setRecipient('')
    await refresh()
  })
  const approve = (requestId: string) => void perform(async () => {
    const issued = unwrap(await client.classroomApprove({ workspace_id: workspaceId }, requestId))
    setEntry(issued.entry_id)
    await refresh()
  })
  const revoke = (entryId: string) => void perform(async () => {
    unwrap(await client.classroomRevoke({ workspace_id: workspaceId }, entryId))
    if (entryId === entry) setEntry(null)
    await refresh()
  })

  return (
    <div className="aiws-share-link-content" data-testid="aiws-classroom-share">
      <section className="aiws-share-section">
        <h3>{t('school.classroom.invite', 'One-use invitation')}</h3>
        {!surfaceId && <p className="aiws-muted">{t('school.classroom.chooseCanvas', 'Open the canvas you want to share.')}</p>}
        <form className="space-y-2" onSubmit={(event) => { event.preventDefault(); issue() }}>
          <label className="block">
            {t('school.classroom.account', 'BuckyOS account ID')}
            <input className="block w-full" value={recipient} onChange={(event) => setRecipient(event.target.value)} maxLength={128} required />
          </label>
          <label className="block">
            {t('school.classroom.access', 'Access')}
            <select className="ml-2" value={kind} onChange={(event) => setKind(event.target.value === 'student' ? 'student' : 'guest')}>
              <option value="guest">{t('school.classroom.guest', 'Guest · read only')}</option>
              <option value="student">{t('school.classroom.student', 'Student · collaborate')}</option>
            </select>
          </label>
          <button className="is-primary" disabled={busy || !surfaceId || !recipient.trim()} type="submit">{t('school.classroom.issue', 'Create invitation')}</button>
        </form>
        {entry && (
          <div className="aiws-share-link-row">
            <input readOnly aria-label={t('school.classroom.invite', 'One-use invitation')} value={entryUrl(entry)} onFocus={(event) => event.target.select()} />
            <button type="button" disabled={busy} onClick={() => void perform(() => navigator.clipboard.writeText(entryUrl(entry)))}>
              {t('school.classroom.copyInvite', 'Copy invitation link')}
            </button>
          </div>
        )}
        <ul className="space-y-2">
          {invitations.filter((item) => item.state !== 'closed').map((item) => (
            <li key={item.entry_id} className="flex flex-wrap items-center gap-2">
              <span>{item.recipient} · {item.scope_id}</span>
              <span>{item.kind === 'guest' ? t('school.classroom.guest', 'Guest · read only') : t('school.classroom.student', 'Student · collaborate')}</span>
              {item.state === 'active' && <button type="button" onClick={() => setEntry(item.entry_id)}>{t('school.classroom.invite', 'One-use invitation')}</button>}
              <button disabled={busy} type="button" onClick={() => revoke(item.entry_id)}>{t('school.classroom.revoke', 'Revoke')}</button>
            </li>
          ))}
        </ul>
      </section>
      <section className="aiws-share-section">
        <h3>{t('school.classroom.requests', 'Read access requests')}</h3>
        {requests.length === 0 && <p className="aiws-muted">{t('school.classroom.noRequests', 'No requests yet.')}</p>}
        <ul className="space-y-2">
          {requests.map(({ entry_id: requestEntry, ...request }) => (
            <li key={request.request_id} className="flex flex-wrap items-center gap-2">
              <span>{request.applicant} · {request.scope_id}</span>
              {request.state === 'pending' ? (
                <button disabled={busy} type="button" onClick={() => approve(request.request_id)}>{t('school.classroom.approve', 'Approve read access')}</button>
              ) : requestEntry && (request.state === 'ready' || request.state === 'approved') ? (
                <>
                  {request.state === 'ready' && <button type="button" onClick={() => setEntry(requestEntry)}>{t('school.classroom.invite', 'One-use invitation')}</button>}
                  <button disabled={busy} type="button" onClick={() => revoke(requestEntry)}>{t('school.classroom.revoke', 'Revoke')}</button>
                </>
              ) : <span className="aiws-muted">{t('school.classroom.closed', 'Closed')}</span>}
            </li>
          ))}
        </ul>
      </section>
      {failed && <p role="alert" className="aiws-error">{t('school.classroom.unavailable', 'Classroom access is unavailable.')}</p>}
    </div>
  )
}
