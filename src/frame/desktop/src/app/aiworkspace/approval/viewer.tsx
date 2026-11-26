import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type MouseEvent } from 'react'
import { createRoot } from 'react-dom/client'
import { FileText, Maximize, MousePointer2, Move, ZoomIn, ZoomOut } from 'lucide-react'
import '../../../index.css'
import '../aiworkspace.css'
import './viewer.css'
import { ContentPreview } from '../../../components/ContentPreview'
import type { ContentRef } from '../../../components/preview/types'
import { I18nProvider } from '../../../i18n/provider'
import { PrototypeThemeProvider } from '../../../theme/provider'
import type { AssetContent } from '../api/types'
import { loadCore } from '../api/wasm'
import { setAiwsLocale } from '../i18n'
import { PrintSession, type SnapshotAssets } from '../print/renderer/session'
import { loadLoro } from '../richtext/loro'
import { StoreContext, WorkspaceUiContext, type WorkspaceUi } from '../state/hooks'
import { RunModeContext } from '../state/runMode'
import { WorkspaceStore } from '../state/store'
import { BlockHost } from '../ui/blocks/BlockHost'
import { registerBuiltinBlocks } from '../ui/blocks/builtin'
import { BudgetContext, createBudget } from '../ui/blocks/budget'
import { requestIntent } from '../ui/blocks/editorToolbar'
import { blockRegistry } from '../ui/blocks/registry'
import { FlowSurface } from '../ui/canvas/FlowSurface'
import { connectorDefinition } from '../ui/canvas/connectors/definition'
import { surfaceBounds } from '../ui/canvas/layout'
import { Camera } from '../ui/canvas/render/camera'
import { RenderHost } from '../ui/canvas/render/RenderHost'
import { useSurfaceLayout } from '../ui/canvas/useSurfaceLayout'
import { registerControls } from '../ui/controls/registerControls'
import { assetSource } from '../ui/media/source'
import { inkDefinition } from '../ink/inkBlock'
import { ApprovalReadOnlyContext } from './readOnly'
import { readApprovalManifest, sameOriginUrl, VIEWER_BOOT, type ApprovalAsset, type ApprovalCanvasManifest,
  type ApprovalCanvasOptions, type ViewerWindow } from './contract'

const noEdit = () => undefined
const noLayout = () => false
const SAFE_PREVIEW = new Set([
  'image/png', 'image/jpeg', 'image/gif', 'image/webp', 'image/avif', 'image/bmp',
  'application/pdf', 'text/plain', 'text/csv', 'application/json',
  'audio/mpeg', 'audio/ogg', 'audio/wav', 'audio/mp4', 'video/mp4', 'video/webm', 'video/ogg',
])

function assetById(records: ApprovalAsset[]): Map<string, ApprovalAsset> {
  return new Map(records.map((item) => [item.object_id, item]))
}

/** The only file reader is this exact approved manifest and the current PBL session. */
function capturedAssets(manifest: ApprovalCanvasManifest, mediaUrl: ApprovalCanvasOptions['mediaUrl'], signal: AbortSignal): SnapshotAssets {
  const known = assetById(manifest.assets)
  const locate = (id: string, inspect: boolean) => {
    if (!known.has(id) || signal.aborted) return null
    return sameOriginUrl(mediaUrl(id, inspect ? { inspect: true } : undefined), window.location)
  }
  return {
    url: (id) => locate(id, false),
    fetch: async (id) => {
      const asset = known.get(id)
      const url = locate(id, true)
      if (!asset || !url || !SAFE_PREVIEW.has(asset.media_type.toLowerCase())) {
        throw new Error('This captured file is not available for preview')
      }
      const response = await fetch(url, { signal, credentials: 'same-origin', redirect: 'error', cache: 'no-store' })
      if (!response.ok || response.headers.get('content-type')?.toLowerCase().includes('text/html')) {
        throw new Error('The captured file could not be opened')
      }
      const data = await response.blob()
      if (data.size !== asset.size) throw new Error('The captured file no longer matches this approval')
      return data.type === asset.media_type ? data : new Blob([data], { type: asset.media_type })
    },
  }
}

function registerApprovalBlocks() {
  registerBuiltinBlocks()
  blockRegistry.register(connectorDefinition)
  blockRegistry.register(inkDefinition)
  registerControls()
}

function FreeCanvas({ surfaceId, selection, choose, preview }: {
  surfaceId: string
  selection: ReadonlySet<string>
  choose: (items: Set<string>) => void
  preview: (id: string) => void
}) {
  const store = useRef<HTMLDivElement>(null)
  const [camera] = useState(() => new Camera())
  const [tool, setTool] = useState<'select' | 'hand'>('select')
  const { laid, index, shapeBook } = useSurfaceLayout(surfaceId)
  const bounds = useMemo(() => surfaceBounds(laid), [laid])
  const latest = useRef(bounds)
  const fitted = useRef(false)
  useLayoutEffect(() => { latest.current = bounds }, [bounds])
  const fit = useCallback(() => {
    const node = store.current
    if (!node?.clientWidth || !node.clientHeight || !latest.current) return false
    camera.setViewportSize(node.clientWidth, node.clientHeight)
    camera.fit(latest.current, 44)
    return true
  }, [camera])
  useLayoutEffect(() => {
    const node = store.current
    if (!node) return
    const remeasure = () => {
      if (!fitted.current) fitted.current = fit()
    }
    remeasure()
    const observer = new ResizeObserver(remeasure)
    observer.observe(node)
    return () => observer.disconnect()
  }, [bounds, fit])
  useEffect(() => () => camera.stopAnimation(), [camera])
  const one = selection.size === 1 ? selection.values().next().value : undefined
  const fileBlock = one && laid.get(one)?.entity.view_type === 'asset' ? one : null

  return (
    <div ref={store} className="approval-native-viewport" data-testid="approval-canvas-free">
      <RenderHost surfaceId={surfaceId} mode="view" camera={camera} laid={laid} index={index}
        selection={selection} onSelectionChange={choose} editing={null} onEditingChange={noEdit}
        canLayout={noLayout} onCommitLayout={noEdit} onContextMenu={noEdit} onOpenEntity={preview}
        tool={tool} formOf={(id) => shapeBook.get(laid.get(id)?.entity)} />
      <div className="approval-native-tools" role="group" aria-label="画布查看工具">
        <button type="button" title={tool === 'hand' ? '选择' : '平移'} aria-label={tool === 'hand' ? '选择' : '平移'}
          aria-pressed={tool === 'hand'} onClick={() => setTool((mode) => mode === 'hand' ? 'select' : 'hand')}>
          {tool === 'hand' ? <MousePointer2 size={17} /> : <Move size={17} />}
        </button>
        <button type="button" title="缩小" aria-label="缩小" onClick={() => camera.zoomTo(camera.zoom / 1.22)}><ZoomOut size={17} /></button>
        <button type="button" title="放大" aria-label="放大" onClick={() => camera.zoomTo(camera.zoom * 1.22)}><ZoomIn size={17} /></button>
        <button type="button" title="适配画布" aria-label="适配画布" onClick={fit}><Maximize size={17} /></button>
        {fileBlock && <button type="button" title="查看文件" aria-label="查看文件" onClick={() => requestIntent(fileBlock, 'preview')}><FileText size={17} /></button>}
      </div>
    </div>
  )
}

function SurfaceView({ store, manifest }: { store: WorkspaceStore; manifest: ApprovalCanvasManifest }) {
  const [selection, choose] = useState<ReadonlySet<string>>(() => new Set())
  const [source, setSource] = useState<ContentRef | null>(null)
  const [budget] = useState(() => createBudget({ editors: 0, html: 0 }))
  const fileScope = useMemo(() => assetById(manifest.assets), [manifest.assets])
  const preview = useCallback((id: string) => {
    void (async () => {
      const candidate = store.outline.get(id)
      const asset = await store.session.read<AssetContent>(candidate?.source_id ?? id)
      if (asset.type_id !== 'buckyos.asset-ref') return
      const payload = asset.content.payload
      const allowed = fileScope.get(payload.object_id)
      if (!allowed || !SAFE_PREVIEW.has(allowed.media_type.toLowerCase())) return
      setSource(assetSource(store, { ...payload, file_name: allowed.name, media_type: allowed.media_type }, allowed.name))
    })().catch(() => undefined)
  }, [store, fileScope])

  const ui = useMemo<WorkspaceUi>(() => {
    const entities = store.outline.all()
    return {
      entities, byId: new Map(entities.map((entity) => [entity.entity_id, entity])),
      annotations: [], openEntity: preview, annotate: null, activeAnnotation: null,
      setActiveAnnotation: noEdit, annotationFilter: null, showAnnotations: noEdit,
      renderCell: (id, depth) => <BlockHost cellId={id} depth={depth} mode="view" view="canvas" />,
    }
  }, [store, preview])
  const blockLinks = (event: MouseEvent) => {
    if (event.target instanceof Element && event.target.closest('a[href]')) {
      event.preventDefault()
      event.stopPropagation()
    }
  }

  return (
    <StoreContext.Provider value={store}>
      <WorkspaceUiContext.Provider value={ui}>
        <BudgetContext.Provider value={budget}>
          <RunModeContext.Provider value={{ interactive: false }}>
            <ApprovalReadOnlyContext.Provider value>
              <main className="aiws-root approval-native" aria-label="审批画布" onClickCapture={blockLinks} onAuxClickCapture={blockLinks}>
                {manifest.mode === 'free' ? (
                  <FreeCanvas surfaceId={manifest.surface_id} selection={selection} choose={choose} preview={preview} />
                ) : (
                  <div className="aiws-flow-host approval-native-flow" data-testid="approval-canvas-flow">
                    <FlowSurface surfaceId={manifest.surface_id} mode="view" selected={selection} onSelect={choose}
                      onInsertMenu={noEdit} insertAt={noEdit} insertGroup={noEdit} onBlockMenu={noEdit} />
                  </div>
                )}
                {source && (
                  <div role="dialog" aria-modal="true" aria-label="文件预览" className="approval-native-preview">
                    <ContentPreview source={source} uiMode="visible" autoFocus allowExport={false} forcePdfJs
                      prefetchAdjacent={false} onRequestExit={() => setSource(null)} />
                  </div>
                )}
              </main>
            </ApprovalReadOnlyContext.Provider>
          </RunModeContext.Provider>
        </BudgetContext.Provider>
      </WorkspaceUiContext.Provider>
    </StoreContext.Provider>
  )
}

function mountViewer(options: ApprovalCanvasOptions): () => void {
  const host = document.getElementById('approval-canvas')
  if (!host) throw new Error('Approval viewer is missing its container')
  const controller = new AbortController()
  const root = createRoot(host)
  let workspace: WorkspaceStore | undefined
  let disposed = false

  const dispose = () => {
    if (disposed) return
    disposed = true
    controller.abort()
    options.signal?.removeEventListener('abort', dispose)
    root.unmount()
    void workspace?.dispose().catch(() => undefined)
  }
  if (options.signal?.aborted) { dispose(); return dispose }
  options.signal?.addEventListener('abort', dispose, { once: true })
  root.render(<p role="status" className="approval-native-state">正在读取审批画布…</p>)

  void (async () => {
    const manifestUrl = sameOriginUrl(options.manifestUrl, window.location)
    const response = await fetch(manifestUrl, { signal: controller.signal, credentials: 'same-origin', redirect: 'error', cache: 'no-store' })
    if (!response.ok) throw new Error('Approval canvas unavailable')
    const manifest = readApprovalManifest(await response.json())
    const [core] = await Promise.all([loadCore(), loadLoro()])
    if (disposed) return
    setAiwsLocale('zh-CN')
    registerApprovalBlocks()
    const session = new PrintSession(core, manifest, capturedAssets(manifest, options.mediaUrl, controller.signal))
    workspace = new WorkspaceStore(session, core)
    await workspace.outline.reload()
    if (disposed) return
    const surface = workspace.outline.get(manifest.surface_id)
    if (!surface || surface.kind !== 'surface') throw new Error('The submitted canvas is absent')
    root.render(
      <PrototypeThemeProvider mode="light">
        <I18nProvider locale="zh-CN">
          <SurfaceView store={workspace} manifest={manifest} />
        </I18nProvider>
      </PrototypeThemeProvider>,
    )
  })().catch(() => {
    if (disposed) return
    root.render(<p role="alert" className="approval-native-state">无法读取当前申报的审批画布。</p>)
  })
  return dispose
}

;(window as ViewerWindow)[VIEWER_BOOT] = mountViewer
