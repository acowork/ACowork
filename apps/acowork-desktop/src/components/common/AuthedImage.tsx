/**
 * Authenticated image loading (ADR-076 follow-up).
 *
 * The Gateway gates every `/api/*` route — the avatar byte routes included —
 * on `Authorization: Bearer <access_token>`. A bare `<img src="…">` cannot
 * carry that header: the WebView issues the request itself, outside
 * `window.fetch`, so the interceptor in [authFetch.ts](../../lib/authFetch.ts)
 * never sees it. Every custom avatar therefore came back 401 and silently
 * degraded to its builtin icon.
 *
 * `AuthedImage` fetches the bytes through `window.fetch` (token attached and
 * rotated by the interceptor) and renders them from a `blob:` URL. Repeat
 * mounts stay cheap — the fetch is backed by the browser's HTTP cache, which
 * the Gateway's avatar routes populate with `Cache-Control: max-age=…`.
 *
 * While the bytes are in flight, and whenever the fetch fails, `fallback`
 * renders instead — the same "no custom avatar" signal a broken `<img>`
 * used to produce.
 *
 * ponytail: no blob cache — one fetch (HTTP-cached) + one object URL per
 * mount, revoked on unmount. The ceiling is that a re-uploaded file
 * re-using the same name stays stale for the Gateway's `max-age=300`
 * window; every upload in practice picks a fresh `avatar-NN` name
 * (`nextAvatarName`), so it does not bite. Add a URL-keyed cache in this
 * module if that ever changes.
 */

import { useEffect, useState, type CSSProperties, type ReactNode } from "react";

import { log } from "../../lib/logger";

export interface AuthedImageProps {
  /** HTTP URL that needs the caller's bearer token. Falsy renders `fallback`. */
  src: string | null | undefined;
  alt?: string;
  className?: string;
  style?: CSSProperties;
  onClick?: () => void;
  /** Rendered while loading and when the fetch fails. */
  fallback?: ReactNode;
}

export function AuthedImage({
  src,
  alt,
  className,
  style,
  onClick,
  fallback,
}: AuthedImageProps) {
  const resolved = useAuthedImageSrc(src ?? null);
  if (!resolved) return <>{fallback ?? null}</>;
  return (
    <img
      src={resolved}
      alt={alt}
      draggable={false}
      className={className}
      style={style}
      onClick={onClick}
    />
  );
}

/**
 * Fetch `url` with the caller's credentials and return a `blob:` URL to
 * render. `null` while loading and on every failure, so the caller can pick
 * its own fallback.
 */
export function useAuthedImageSrc(url: string | null): string | null {
  const [resolved, setResolved] = useState<string | null>(null);

  useEffect(() => {
    // The previous effect's cleanup already revoked the old blob URL, so a
    // stale `resolved` would be a dangling src — clear it before refetching.
    setResolved(null);
    if (!url) return;

    let cancelled = false;
    let objectUrl: string | null = null;
    fetch(url)
      .then((resp) => {
        // A bare `<img>` failing with 401 is precisely the bug this
        // component exists to prevent (see module docs). Staying silent
        // here made that regression indistinguishable from "no custom
        // avatar configured" — both render `fallback`. Log the status so
        // the two are told apart in the Desktop log.
        if (!resp.ok) {
          log.warn(
            `[AuthedImage] avatar fetch failed: status=${resp.status} url=${url}`,
          );
          return null;
        }
        return resp.blob();
      })
      .then((blob) => {
        if (!blob || cancelled) return;
        objectUrl = URL.createObjectURL(blob);
        setResolved(objectUrl);
      })
      .catch((err) => {
        // Offline or unauthorized — the caller's fallback is the answer.
        log.warn(`[AuthedImage] avatar fetch threw: url=${url}`, err);
      });

    return () => {
      cancelled = true;
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [url]);

  return resolved;
}
