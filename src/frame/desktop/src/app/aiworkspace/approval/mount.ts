import { sameOriginUrl, VIEWER_BOOT, type ApprovalCanvasOptions, type ViewerWindow } from './contract'

/** The iframe isolates the viewer's native desktop CSS and preview portals from PBL's page. */
export function mountApprovalCanvas(host: HTMLElement, options: ApprovalCanvasOptions): () => void {
  if (options.signal?.aborted) return () => undefined
  const origin = host.ownerDocument.defaultView?.location
  if (!origin) throw new Error('Approval viewer host is detached')
  const manifestUrl = sameOriginUrl(options.manifestUrl, origin)
  const controller = new AbortController()
  const frame = host.ownerDocument.createElement('iframe')
  frame.className = 'bucky-approval-canvas'
  frame.title = '审批画布'
  frame.setAttribute('sandbox', 'allow-scripts allow-same-origin')
  frame.setAttribute('referrerpolicy', 'no-referrer')
  frame.setAttribute('allow', 'fullscreen')
  frame.style.cssText = 'width:100%;height:100%;border:0;display:block;color-scheme:light'
  const viewerUrl = new URL(manifestUrl)
  if (!/^\/xmz\/api\/projects\/[^/]+\/approval-workspace\/manifest$/.test(viewerUrl.pathname)) {
    throw new Error('Approval manifest is outside the project preview route')
  }
  viewerUrl.pathname = viewerUrl.pathname.replace(/\/manifest$/, '/viewer')
  frame.src = viewerUrl.href
  let cleanup: (() => void) | undefined

  const dispose = () => {
    options.signal?.removeEventListener('abort', dispose)
    controller.abort()
    cleanup?.()
    cleanup = undefined
    frame.remove()
  }
  frame.addEventListener('load', () => {
    if (controller.signal.aborted) return
    try {
      const mount = (frame.contentWindow as ViewerWindow | null)?.[VIEWER_BOOT]
      if (!mount) throw new Error('Viewer entry unavailable')
      cleanup?.()
      cleanup = mount({
        signal: controller.signal,
        manifestUrl,
        mediaUrl: (id, flags) => sameOriginUrl(options.mediaUrl(id, flags), origin),
      })
    } catch {
      const message = host.ownerDocument.createElement('p')
      message.setAttribute('role', 'alert')
      message.textContent = '无法打开审批画布。'
      if (frame.isConnected) frame.replaceWith(message)
      controller.signal.addEventListener('abort', () => message.remove(), { once: true })
    }
  })
  options.signal?.addEventListener('abort', dispose, { once: true })
  host.append(frame)
  return dispose
}

export type { ApprovalCanvasOptions } from './contract'
