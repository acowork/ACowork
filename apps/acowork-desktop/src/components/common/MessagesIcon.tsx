/**
 * Outline / filled message-bubble icons for the user inbox nav item
 * (ADR-076 §决策 8).
 *
 * Deliberately *not* the same silhouette as [ChatIcon](ChatIcon.tsx) (that
 * one is the agent-session bubble): two nav targets one pixel apart in the
 * same rail need to be distinguishable at 24px, so this is a square bubble
 * with two text lines.
 */

export function OutlineMessagesIcon({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.75"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d="M21 15a2 2 0 0 1-2 2H8l-4 4V5a2 2 0 0 1 2-2h13a2 2 0 0 1 2 2z" />
      <line x1="8" y1="9" x2="16" y2="9" />
      <line x1="8" y1="12.5" x2="13" y2="12.5" />
    </svg>
  );
}

export function FilledMessagesIcon({ className }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="currentColor">
      <path d="M21 15a2 2 0 0 1-2 2H8l-4 4V5a2 2 0 0 1 2-2h13a2 2 0 0 1 2 2z" />
    </svg>
  );
}
