import type { ReplicaRows } from '@print/package.ts'

export interface ApprovalCanvasOptions {
  manifestUrl: string
  mediaUrl: (objectId: string, options?: { inspect?: boolean }) => string
  signal?: AbortSignal
}

export interface ApprovalAsset {
  object_id: string
  name: string
  media_type: string
  size: number
}

export interface ApprovalCanvasManifest {
  schema_version: 2
  project_id: string
  workspace_url: string
  workspace_id: string
  surface_id: string
  epoch: string
  head_seq: number
  captured_at: string
  title: string
  mode: 'free' | 'flow'
  principal: 'pbl-approval-reviewer'
  rows: ReplicaRows
  assets: ApprovalAsset[]
}

const fields = ['project_id', 'workspace_url', 'workspace_id', 'surface_id', 'epoch', 'captured_at', 'title'] as const

function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

export function readApprovalManifest(value: unknown): ApprovalCanvasManifest {
  if (!record(value) || value.schema_version !== 2 || value.principal !== 'pbl-approval-reviewer'
      || fields.some((field) => typeof value[field] !== 'string')
      || !value.workspace_id || !value.surface_id || !value.epoch
      || typeof value.head_seq !== 'number' || !Number.isSafeInteger(value.head_seq) || value.head_seq < 0
      || (value.mode !== 'free' && value.mode !== 'flow')
      || !record(value.rows) || !Array.isArray(value.rows.entities)
      || !Object.values(value.rows).every(Array.isArray)
      || !Array.isArray(value.assets)) {
    throw new Error('Invalid approval canvas manifest')
  }
  const ids = new Set<string>()
  for (const asset of value.assets) {
    if (!record(asset) || typeof asset.object_id !== 'string' || !asset.object_id
        || typeof asset.name !== 'string' || typeof asset.media_type !== 'string'
        || typeof asset.size !== 'number' || !Number.isSafeInteger(asset.size) || asset.size < 0
        || ids.has(asset.object_id)) {
      throw new Error('Invalid approval asset manifest')
    }
    ids.add(asset.object_id)
  }
  return value as unknown as ApprovalCanvasManifest
}

export function sameOriginUrl(value: string, location: Location): string {
  const url = new URL(value, location.href)
  if (!['http:', 'https:'].includes(url.protocol) || url.origin !== location.origin
      || url.username || url.password || url.hash) {
    throw new Error('Approval viewer resource must be same-origin')
  }
  return url.href
}

export const VIEWER_BOOT = '__BUCKY_APPROVAL_VIEWER_MOUNT__'
type ViewerMount = (options: ApprovalCanvasOptions) => () => void
export type ViewerWindow = Window & { [VIEWER_BOOT]?: ViewerMount }
