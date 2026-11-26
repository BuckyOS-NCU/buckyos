import { getBootIdentity } from '../../sso/identity'
import { workspaceIdOfHost, zoneOrigin } from './links'

const PROJECT_PAGE = 'https://139.196.234.199/xmz/'
const CHOICE_FRAGMENT = 'pbl_canvas'
const CHOICE_TOKEN = /^[a-zA-Z0-9_-]{43}$/

export function projectCanvasChoice(): string | null {
  const identity = getBootIdentity()
  if (zoneOrigin() !== import.meta.env.VITE_NCU_CAS_ORIGIN
      || identity.kind !== 'account' || String(identity.auth_method) !== 'ncu_pbl_teacher') return null
  const params = new URLSearchParams(window.location.hash.slice(1))
  const state = params.get(CHOICE_FRAGMENT)
  return state && params.size === 1 && CHOICE_TOKEN.test(state) ? state : null
}

export function carryProjectCanvasChoice(address: string): string {
  const state = projectCanvasChoice()
  if (!state) return address
  const url = new URL(address)
  const workspaceId = workspaceIdOfHost(url.host)
  if (!workspaceId || url.origin !== `https://aiws-${workspaceId.slice(3)}.${new URL(zoneOrigin()).host}`) return address
  url.hash = new URLSearchParams({ [CHOICE_FRAGMENT]: state }).toString()
  return url.href
}

export function returnProjectCanvasChoice(state: string, canvasUrl?: string): void {
  const params = new URLSearchParams({ state })
  if (canvasUrl) params.set('workspace_url', canvasUrl)
  window.location.replace(`${PROJECT_PAGE}#canvas-return?${params}`)
}
