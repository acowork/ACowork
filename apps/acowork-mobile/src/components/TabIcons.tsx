/**
 * Tab-bar icons — copied verbatim from the Desktop left NavBar so the two
 * apps share one visual language. Each nav item has an outline (inactive)
 * and a filled (active) variant of the SAME silhouette, so toggling the
 * selection changes only the fill, not the shape.
 *
 * Filled variants use `fill="currentColor"` (with white "carved" inner
 * strokes where needed) so they pick up the tab-bar tint colour when active.
 *
 * Stroke width is dialled down from the Desktop's 1.75 to 1.5: these render
 * at 26px on a phone, where 1.75 reads a touch heavy next to the Settings
 * list icons (lucide strokeWidth 2 @ 22px ≈ 1.83px on screen). 1.5 ≈ 1.63px.
 */
import { useId } from 'react'

type IconProps = { className?: string }

/* --- Chat --------------------------------------------------------------- */

export function OutlineChatIcon({ className }: IconProps) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
      <g transform="translate(1.2, 1.2) scale(0.9)">
        <path d="M12 3C6.5 3 2 7.1 2 12c0 2.5 1.1 4.8 2.9 6.5L3 22l5.3-2.3C9.6 20.5 11.2 21 13 21c5.5 0 10-4.1 10-9s-4.5-9-11-9z" />
      </g>
    </svg>
  )
}

export function FilledChatIcon({ className }: IconProps) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="currentColor">
      <g transform="translate(1.2, 1.2) scale(0.9)">
        <path d="M12 3C6.5 3 2 7.1 2 12c0 2.5 1.1 4.8 2.9 6.5L3 22l5.3-2.3C9.6 20.5 11.2 21 13 21c5.5 0 10-4.1 10-9s-4.5-9-11-9z" />
      </g>
    </svg>
  )
}

/* --- Projects (Gantt board) -------------------------------------------- */

export function OutlineProjectsIcon({ className }: IconProps) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
      <rect x="3" y="4" width="18" height="16" rx="2" />
      <circle cx="6.5" cy="8.5" r="0.6" />
      <line x1="10.5" y1="8.5" x2="15" y2="8.5" />
      <circle cx="6.5" cy="12" r="0.6" />
      <line x1="10.5" y1="12" x2="18" y2="12" />
      <circle cx="6.5" cy="15.5" r="0.6" />
      <line x1="10.5" y1="15.5" x2="13" y2="15.5" />
    </svg>
  )
}

export function FilledProjectsIcon({ className }: IconProps) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="currentColor" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
      <rect x="3" y="4" width="18" height="16" rx="2" />
      <circle cx="6.5" cy="8.5" r="0.6" fill="white" stroke="white" />
      <line x1="10.5" y1="8.5" x2="15" y2="8.5" stroke="white" />
      <circle cx="6.5" cy="12" r="0.6" fill="white" stroke="white" />
      <line x1="10.5" y1="12" x2="18" y2="12" stroke="white" />
      <circle cx="6.5" cy="15.5" r="0.6" fill="white" stroke="white" />
      <line x1="10.5" y1="15.5" x2="13" y2="15.5" stroke="white" />
    </svg>
  )
}

/* --- Docs (document) --------------------------------------------------- */

export function OutlineDocsIcon({ className }: IconProps) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
      <path d="M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9.5L14 3z" />
      <polyline points="14,3 14,9 20,9" />
      <line x1="8" y1="12" x2="16" y2="12" />
      <line x1="8" y1="15" x2="12" y2="15" />
    </svg>
  )
}

export function FilledDocsIcon({ className }: IconProps) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="currentColor" stroke="currentColor" strokeWidth="1.5">
      <path d="M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9.5L14 3z" />
      <polyline points="14,3 14,9 20,9" fill="none" stroke="white" />
      <line x1="8" y1="12" x2="16" y2="12" stroke="white" />
      <line x1="8" y1="15" x2="12" y2="15" stroke="white" />
    </svg>
  )
}

/* --- Settings (gear) --------------------------------------------------- */

export function OutlineSettingsIcon({ className }: IconProps) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
      <path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" />
      <circle cx="12" cy="12" r="3" />
    </svg>
  )
}

export function FilledSettingsIcon({ className }: IconProps) {
  const maskId = useId()
  return (
    <svg className={className} viewBox="0 0 24 24" fill="currentColor" stroke="currentColor" strokeWidth="2">
      <defs>
        <mask id={maskId}>
          <rect width="24" height="24" fill="white" />
          <circle cx="12" cy="12" r="3" fill="black" />
        </mask>
      </defs>
      <g mask={`url(#${maskId})`}>
        <path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" />
      </g>
    </svg>
  )
}
