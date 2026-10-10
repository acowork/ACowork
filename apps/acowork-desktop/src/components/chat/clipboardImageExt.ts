/**
 * Map a raster MIME to a file extension (lowercase, no dot).
 *
 * Used by the chat panel's paste handler when it bridges a clipboard
 * image Blob through the Rust temp-file writer — the writer needs an
 * extension to form the temp path, and the runtime's `upload_file`
 * pipeline reads the extension to classify the attachment (image vs
 * document).
 *
 * Kept in its own module so it can be unit-tested without dragging
 * the whole ChatPanel dependency graph into the test runner.
 */
export function extensionForMime(mime: string): string {
  switch (mime.toLowerCase()) {
    case "image/png":
      return "png";
    case "image/jpeg":
    case "image/jpg":
      return "jpg";
    case "image/gif":
      return "gif";
    case "image/webp":
      return "webp";
    case "image/bmp":
      return "bmp";
    default: {
      // Fallback: take whatever sits after the slash, sanitised.
      // Strip non-alphanumerics in case the MIME has a charset
      // suffix or vendor prefix.
      const tail = mime.split("/")[1] ?? "";
      return tail.replace(/[^a-z0-9]/gi, "").toLowerCase() || "png";
    }
  }
}

/**
 * Encode a Blob as a standard (padded) base64 string, without the
 * `data:...;base64,` prefix.
 *
 * Used to ship clipboard image bytes across the Tauri IPC. Deliberately
 * built on `FileReader.readAsDataURL` rather than `btoa(String.fromCharCode(...chunk))`:
 * the spread form overflows the native call-argument limit on multi-MB
 * buffers unless you hand-chunk it, and `readAsDataURL` is native,
 * async, and size-agnostic.
 *
 * Rejects rather than resolving `""` so the caller's catch path (fall
 * through to a no-op paste) is exercised instead of writing an empty
 * temp file.
 */
export function blobToBase64(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const result = reader.result;
      if (typeof result !== "string") {
        reject(new Error("FileReader returned a non-string result"));
        return;
      }
      // `data:<mime>;base64,<payload>` — strip everything up to and
      // including the FIRST comma (base64's alphabet never contains one,
      // so indexOf is sufficient and avoids a full split).
      const comma = result.indexOf(",");
      if (comma === -1) {
        reject(new Error("Malformed data URL from FileReader"));
        return;
      }
      resolve(result.slice(comma + 1));
    };
    reader.onerror = () => reject(reader.error ?? new Error("FileReader failed"));
    reader.readAsDataURL(blob);
  });
}
