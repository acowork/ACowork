# Icons

`32x32.png` … `512x512.png` are committed **placeholders**: flat
`#007aff` squares, not artwork. They exist so `tauri build` and
`npm run tauri:dev` work from a clean checkout before anyone opens a design
tool.

Regenerate the real set (and the `.icns` / `.ico` that macOS and Windows
bundling want) from one 1024×1024 master:

```bash
npx tauri icon path/to/master-1024.png
```

That command rewrites this directory in place, so the placeholders should be
replaced in the same commit that introduces real artwork — not left to rot
next to it.
