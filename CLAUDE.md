# CLAUDE.md — RollVault

DIT offload / read-back verify / ASC MHL / camera reports. macOS-only desktop app.

## Commands
- Dev app: `npm run tauri dev` (Vite on :1420, auto-started by Tauri)
- Dev UI only (mock backend): `npm run dev`
- Build app: `npm run tauri build` → .app + .dmg
- Build/typecheck frontend: `npm run build` (`tsc && vite build`)
- Test engine: `npm run test:core` = `cargo test -p rollvault-core -- --test-threads=4` (uses real `hdiutil` disk images, slow)
- Check Rust: `cargo check --workspace` · `cargo clippy --workspace`
- CLI front end: `cargo run -p rollvault-core --bin rollvault-cli -- <args>`
- No linter/formatter configured for TS. Debug logs: `RV_DEBUG=1`.

## Stack
- Rust 2024, workspace resolver 3: `crates/rollvault-core` (engine, no UI deps) + `src-tauri` (Tauri 2 app)
- Frontend: React 19 + TypeScript (strict, `noEmit`) + Vite 8 + Tailwind 4 (`@tailwindcss/vite`), `lucide-react` icons
- Engine deps: sha2/md-5/xxhash-rust, walkdir, plist, chrono, serde, uuid, libc (F_NOCACHE/F_FULLFSYNC)
- Layout: see README "Project layout". Design tokens: `design-system/rollvault/MASTER.md`. `IMPLEMENTATION.md` = stale design notes (VN), code wins.

## Code rules
1. **Errors are `Result<T, String>`** with a user-facing message — no custom error enums (thiserror unused). Engine messages are built with the `l!("English {}", "Tiếng Việt {}", arg)` macro (`core/l10n.rs`); never hardcode a user-visible English string in the engine.
2. **Every `#[tauri::command]` is `async` and returns `Result<_, String>`**; anything touching a disk goes through `off_main(…)` / `spawn_blocking` (Tauri runs sync commands on the UI thread).
3. **All blocking I/O is time-boxed**: wrap in `util::run_with_timeout`, `util::output_with_timeout`, or `util::supervised` (stall watchdog) — a yanked card or hung NAS must never block a thread forever.
4. **IPC contract is manual and mirrored**: a new command needs a Rust command fn + an entry in `src/api.ts` (`api.*`) + a hand-written interface in `src/types.ts` (snake_case fields, matching serde). Mock it in `src/mock.ts` so `npm run dev` still works.
5. **UI text lives only in `src/i18n.tsx`** (parallel `en`/`vi` dicts, values are strings or arg functions). Components are plain functions taking one inline-typed `props` object (no signature destructuring); style with Tailwind classes over design-system CSS vars (`bg-raised`, `text-ink`, `var(--radius-control)`), not raw hex.
- Rust tests go in `#[cfg(test)] mod tests` at the bottom of the module they cover.
