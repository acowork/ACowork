/**
 * Loose extension → Monaco language mapping, shared by every surface that
 * opens a Monaco tab for a path it did not get from the workspace tree:
 * git virtual tabs ([GitStatusPanel](../components/workspace/git/GitStatusPanel.tsx))
 * and chat-attachment previews (inbox thread chips).
 *
 * Deliberately a *loose* map: an unknown extension falls back to
 * `plaintext`, which Monaco renders fine, so a missed entry degrades to
 * "no syntax colours" rather than a broken tab. This is not the file tree's
 * icon mapping (Seti UI) and not the LSP language id — see
 * `lspUtils.toLspLanguageId` for the Monaco → LSP translation.
 */

const LANGUAGE_BY_EXT: Record<string, string> = {
  ts: "typescript",
  tsx: "typescript",
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  json: "json",
  md: "markdown",
  markdown: "markdown",
  rs: "rust",
  py: "python",
  go: "go",
  java: "java",
  kt: "kotlin",
  c: "c",
  h: "c",
  cpp: "cpp",
  cc: "cpp",
  hpp: "cpp",
  cs: "csharp",
  rb: "ruby",
  php: "php",
  swift: "swift",
  css: "css",
  scss: "scss",
  less: "less",
  html: "html",
  htm: "html",
  xml: "xml",
  yaml: "yaml",
  yml: "yaml",
  sh: "shell",
  bash: "shell",
  zsh: "shell",
  ps1: "powershell",
  toml: "ini",
  ini: "ini",
  sql: "sql",
};

/**
 * Extensions that are plain text but have no Monaco language of their own.
 *
 * Only consulted by [`isTextReadablePath`] — a file here previews as
 * `plaintext` rather than being refused a preview.
 */
const PLAIN_TEXT_EXT = new Set([
  "txt",
  "text",
  "log",
  "csv",
  "tsv",
  "env",
  "conf",
  "cfg",
  "properties",
  "patch",
  "diff",
]);

function extensionOf(path: string): string {
  const dot = path.lastIndexOf(".");
  return dot >= 0 ? path.slice(dot + 1).toLowerCase() : "";
}

/** Monaco language id for `path`; `plaintext` when the extension is unknown. */
export function languageForPath(path: string): string {
  return LANGUAGE_BY_EXT[extensionOf(path)] ?? "plaintext";
}

/**
 * Whether `path` names a text file we can render in Monaco at all.
 *
 * This is the whitelist gate (not a guess): it answers "yes" only for
 * extensions we know are text, so callers use it to decide between
 * "open a read-only preview" and "keep the download affordance only".
 * Anything that says no must be treated as opaque bytes — a PDF, a zip, a
 * database dump — never as text.
 */
export function isTextReadablePath(path: string): boolean {
  const ext = extensionOf(path);
  return ext in LANGUAGE_BY_EXT || PLAIN_TEXT_EXT.has(ext);
}
