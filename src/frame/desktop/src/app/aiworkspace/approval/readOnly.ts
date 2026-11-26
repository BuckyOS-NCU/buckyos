import { createContext } from 'react'

/** A captured approval view has no live workspace actions or extension execution. */
export const ApprovalReadOnlyContext = createContext(false)
