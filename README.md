# G AGENT (`v3.1`) — Pure C Native Desktop Autonomous Workspace

**G AGENT** is a standalone 64-bit Windows desktop application (`gagent.exe`) and modular **100% C** autonomous agent runtime (`src/*.c`, `src/gagent.h`). It requires **no Rust**, **no browser**, **no `127.0.0.1` localhost server**, and **no OAuth `Client ID`**.

---

## Key Highlights

1. **100% Pure C Architecture (`src/*.c` & `src/gagent.h`)**
   - Every subsystem (`SelectiveSession`, `ShadowSandbox`, `AllowedFolderBoundary`, `GeminiApiVault`, `DriveZeroDiskBridge`, `LocalToolExecutor`, and `GagentEngine`) is implemented in clean, standard C99 (`src/*.c`) and pre-compiled into `gagent.exe`.
2. **Standalone Win64 Dark-Mode GDI GUI (`gagent.exe`)**
   - Uses native `USER32.dll`, `GDI32.dll`, `COMDLG32.dll`, and `SHELL32.dll` with a double-buffered (`CreateCompatibleBitmap` + `BitBlt`) **Obsidian-Slate (`#070A12`)** renderer and the **G AGENT 3D Hexagonal Wireframe Emblem**.
   - **Zero Browser / Zero Localhost:** Opens directly as a native Windows desktop window in `0.01s`.
3. **Every Button is Interactive & Connected to Real C Functions**
   - **Top Header Bar:** `[ Assistant | Sandbox | Studio ]` mode switcher pills + `[ Self-Heal & Verify ]` 6-link diagnostics button.
   - **Left Navigation Rail:** `[ + New Agent Session ]`, `[ Allowed Folder Guard ]` (runs live traversal block test), `[ Google Drive Zero-Disk ]`, `[ Gemini API Key Vault ]`, `[ ShadowSandbox Rootfs ]`, 3 verified C session presets, and `[ Open Gagent_Workspace ]` / `[ Open .gagent_sandbox ]` Windows Explorer buttons.
   - **Center Command Composer:** Dark API Key input + `[ API Key Linked ]` + `[ Drive: Linked ]`, multi-line Command Composer, `[ + Attach ]` (opens native Windows File Explorer picker `GetOpenFileNameA`), `[ Modules ^ ]` cascading 5-bridge toggle popover, `[ Model: Gemini 2.5 Pro v ]` selector, `[ Run Task -> ]`, and 3 quick-action pills.
   - **Right Sandbox Studio:** Live C code editor with persistent per-tab buffers (`gagent_task.c`, `sandbox_audit.json`, `gagent_config.h`), `[ Verify in Sandbox ]`, `[ Promote to Host ]`, and `[ Restore Snapshot (SelectiveSession Rollback) ]`.

---

## Repository Structure (`100% C`)

```text
Gagent/
├── gagent.exe                        # Pre-compiled 64-bit Windows Native Desktop App (PE32+ GUI)
├── gagent_desktop_blueprint.svg      # Vector UI Blueprint (SVG)
├── Makefile                          # Standard C Makefile (gcc -O2 src/*.c)
├── build_win64.bat                   # Windows C build helper script
├── README.md                         # Project documentation
├── ARCHITECTURE.md                   # Detailed C module & folder guide
├── .gitignore                        # C build ignore rules
└── src/
    ├── gagent.h                      # Master C header (structs, enums, prototypes)
    ├── error.c                       # Fault taxonomy & recovery strategy mapper
    ├── session.c                     # SelectiveSession snapshot & auto-healer
    ├── bootstrap.c                   # AllowedFolderBoundary & .gagent_sandbox initializer
    ├── sandbox.c                     # ShadowSandbox stage, verify & atomic promote engine
    ├── auth.c                        # Direct Gemini API Key Vault (Zero-Client-ID)
    ├── workspace.c                   # Google Drive Zero-Disk Cloud Bridge
    ├── tools.c                       # Atomic file I/O, snapshot rollback & command guard
    ├── engine.c                      # GagentEngine task coordinator & self-diagnostics
    └── main.c                        # Pure C verification & self-test entry point
```
