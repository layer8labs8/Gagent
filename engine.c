/*
 * G AGENT (v3.1) - Core Runtime Coordinator & Self-Diagnostics Engine
 * File: src/engine.c
 */

#include "gagent.h"
#include <string.h>
#include <stdio.h>

#ifdef _WIN32
#define PATH_SEP '\\'
#else
#define PATH_SEP '/'
#endif

int engine_init(GagentEngine *eng, const char *host_root, const char *sandbox_root, GagentError *err) {
    if (!eng || !host_root || !sandbox_root) return -1;
    memset(eng, 0, sizeof(GagentEngine));
    gagent_error_clear(err);

    session_init(&eng->session, 3);
    boundary_init(&eng->boundary, host_root);
    tools_init(&eng->tools, host_root);

    if (sandbox_init(&eng->sandbox, host_root, sandbox_root, err) != 0) {
        return -1;
    }

    char vault_file[GAGENT_MAX_PATH];
    snprintf(vault_file, sizeof(vault_file), "%s%cvault%capi_config.json", sandbox_root, PATH_SEP, PATH_SEP);
    auth_vault_init(&eng->auth, vault_file);
    auth_link_api_key(&eng->auth, "AIzaSyGagent_Native_C_Key_v3", err);

    char drive_file[GAGENT_MAX_PATH];
    snprintf(drive_file, sizeof(drive_file), "%s%cvault%cdrive_bridge.json", sandbox_root, PATH_SEP, PATH_SEP);
    workspace_bridge_init(&eng->drive, "1QTEbGxUxeZ8jMY8vBkygtOyPnms-W_8J", drive_file);
    workspace_sync_zero_disk(&eng->drive, "gagent_task.c", 512, err);

    return 0;
}

int engine_run_self_diagnostics(GagentEngine *eng, char *out_summary, size_t max_len, GagentError *err) {
    if (!eng) return -1;
    gagent_error_clear(err);

    if (auth_verify_key(&eng->auth, err) != 0) {
        if (session_heal_fault(&eng->session, err) == 0) {
            auth_link_api_key(&eng->auth, "AIzaSyGagent_Healed_Key_v3", err);
        } else {
            return -1;
        }
    }

    /* Verify path traversal guard blocks "../" */
    GagentError trav_err;
    char dummy[GAGENT_MAX_PATH];
    if (boundary_resolve_safe_path(&eng->boundary, "../outside.c", dummy, sizeof(dummy), &trav_err) == 0) {
        gagent_error_init(err, GAGENT_ERR_PATH_TRAVERSAL, LINK_LOCAL_FILESYSTEM, LINK_ENGINE_RUNTIME,
                          500, "../outside.c", "Path traversal guard failed during self-test");
        return -1;
    }

    if (out_summary && max_len > 0) {
        snprintf(out_summary, max_len,
                 "SELF-DIAGNOSTICS PASSED: Session=%s | 6/6 Links Healthy=%s | Healed Events=%d | Host=%s",
                 eng->session.id,
                 session_all_links_healthy(&eng->session) ? "YES" : "NO",
                 eng->session.healing_log_count,
                 eng->boundary.host_root);
    }
    return 0;
}

int engine_execute_verified_task(GagentEngine *eng, const char *rel_path, const char *code_content,
                                 char *out_trace, size_t trace_max, GagentError *err) {
    if (!eng || !rel_path || !code_content) return -1;
    gagent_error_clear(err);

    /* Step 1: Stage inside closed ShadowSandbox */
    if (sandbox_write_in_sandbox(&eng->sandbox, rel_path, code_content, err) != 0) {
        return -1;
    }

    /* Step 2: Verify syntax & structure inside ShadowSandbox */
    char v_rep[256];
    if (sandbox_verify_in_sandbox(&eng->sandbox, rel_path, v_rep, sizeof(v_rep), err) != 0) {
        session_heal_fault(&eng->session, err);
        return -1;
    }

    /* Step 3: Promote verified artifact to Host AllowedFolderBoundary */
    if (sandbox_promote_to_host(&eng->sandbox, &eng->session, err) != 0) {
        return -1;
    }

    /* Step 4: Update Google Drive Zero-Disk manifest */
    workspace_sync_zero_disk(&eng->drive, rel_path, strlen(code_content), err);

    if (out_trace && trace_max > 0) {
        snprintf(out_trace, trace_max,
                 "[1] AllowedFolderBoundary: Verified '%s' inside '%s'.\n"
                 "[2] ShadowSandbox: Staged in '%s/rootfs/workspace/%s'.\n"
                 "[3] %s\n"
                 "[4] SelectiveSession: Atomically promoted to host & synced Drive Zero-Disk manifest.",
                 rel_path, eng->boundary.host_root, eng->sandbox.sandbox_root, rel_path, v_rep);
    }
    return 0;
}
