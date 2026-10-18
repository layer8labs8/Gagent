# G AGENT (`v3.1`) — Pure C System Architecture

## 1. Directory & Runtime Layout

When `gagent.exe` runs on Windows, it automatically creates and manages two isolated local directories next to the executable:

1. **`Gagent_Workspace\` (`AllowedFolderBoundary`):**
   - The authorized host output folder where verified C code (`gagent_task.c`, `gagent_config.h`, `sandbox_audit.json`) is promoted ONLY after passing verification inside the sandbox.
   - Protected by `boundary_resolve_safe_path()` in `src/bootstrap.c`, which blocks `..` parent traversal and out-of-tree paths.
2. **`.gagent_sandbox\` (`ShadowSandbox` & `Vault`):**
   - `.gagent_sandbox\rootfs\etc\os-release` — Isolated micro-rootfs descriptor.
   - `.gagent_sandbox\rootfs\workspace\` — Staging area where all code edits and attached files are written and verified first (`sandbox_write_in_sandbox` & `sandbox_verify_in_sandbox`).
   - `.gagent_sandbox\vault\api_config.json` — Stores your linked Gemini API Key (`AIzaSy...`).
   - `.gagent_sandbox\vault\drive_bridge.json` — Stores the Google Drive Zero-Disk synchronization state (`1QTEbGxUxeZ8jMY8vBkygtOyPnms-W_8J`).

---

## 2. C Source Files (`src/`)

| File | Key C Functions & Role |
| :--- | :--- |
| `src/gagent.h` | Declares `GagentEngine`, `SelectiveSession`, `ShadowSandbox`, `AllowedFolderBoundary`, `GeminiApiVault`, `DriveZeroDiskBridge`, and `GagentError`. |
| `src/error.c` | Implements `gagent_error_init()`, `gagent_error_is_recoverable()`, and maps faults to deterministic `RecoveryStrategy` actions. |
| `src/session.c` | Implements `SelectiveSession` (`session_record_snapshot`, `session_get_snapshot`, `session_heal_fault`) for pre-mutation byte snapshots and automatic fault recovery. |
| `src/bootstrap.c` | Implements `boundary_init()`, `boundary_resolve_safe_path()`, and `bootstrap_sandbox_tree()`. |
| `src/sandbox.c` | Implements `sandbox_write_in_sandbox()`, `sandbox_verify_in_sandbox()` (AST brace & syntax verification), and `sandbox_promote_to_host()` (atomic `.tmp` rename). |
| `src/auth.c` | Implements `auth_vault_init()`, `auth_link_api_key()`, and `auth_verify_key()` for direct API key authentication without OAuth `Client ID`. |
| `src/workspace.c` | Implements `workspace_bridge_init()` and `workspace_sync_zero_disk()` for Google Drive Zero-Disk streaming. |
| `src/tools.c` | Implements `tools_write_file_atomic()`, `tools_read_file_safe()`, `tools_rollback_file()`, and `tools_is_command_blocked()`. |
| `src/engine.c` | Implements `engine_init()`, `engine_execute_verified_task()`, and `engine_run_self_diagnostics()`. |
| `src/main.c` | Runs the 6-stage C verification test suite (`make test`). |
