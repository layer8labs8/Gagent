/*
 * G AGENT (v3.1) - Sandboxed Local Filesystem & Command Policy Executor
 * File: src/tools.c
 */

#include "gagent.h"
#include <string.h>
#include <stdio.h>

static const char *BLOCKED_PATTERNS[] = {
    "rm -rf /",
    "mkfs",
    "dd if=",
    ":(){ :|:& };:",
    "format c:",
    NULL
};

void tools_init(LocalToolExecutor *tools, const char *host_root) {
    if (!tools) return;
    memset(tools, 0, sizeof(LocalToolExecutor));
    boundary_init(&tools->boundary, host_root);
    tools->command_timeout_ms = 5000;
}

int tools_is_command_blocked(const char *cmd_line) {
    if (!cmd_line) return 0;
    for (int i = 0; BLOCKED_PATTERNS[i] != NULL; ++i) {
        if (strstr(cmd_line, BLOCKED_PATTERNS[i]) != NULL) {
            return 1;
        }
    }
    return 0;
}

int tools_write_file_atomic(LocalToolExecutor *tools, SelectiveSession *sess,
                            const char *rel_path, const char *content, GagentError *err) {
    if (!tools || !rel_path || !content) return -1;

    char full_path[GAGENT_MAX_PATH];
    if (boundary_resolve_safe_path(&tools->boundary, rel_path, full_path, sizeof(full_path), err) != 0) {
        return -1;
    }

    if (sess) {
        FILE *existing = fopen(full_path, "rb");
        if (existing) {
            char old_buf[GAGENT_MAX_CONTENT];
            size_t n = fread(old_buf, 1, sizeof(old_buf) - 1, existing);
            fclose(existing);
            old_buf[n] = '\0';
            session_record_snapshot(sess, full_path, old_buf, n, 1);
        } else {
            session_record_snapshot(sess, full_path, NULL, 0, 0);
        }
    }

    char tmp_path[GAGENT_MAX_PATH * 2];
    snprintf(tmp_path, sizeof(tmp_path), "%s.gagent.tmp", full_path);

    FILE *f = fopen(tmp_path, "wb");
    if (!f) {
        gagent_error_init(err, GAGENT_ERR_FILESYSTEM, LINK_LOCAL_FILESYSTEM, LINK_ENGINE_RUNTIME,
                          -1, tmp_path, "Failed to open atomic temporary file for write");
        return -1;
    }
    fwrite(content, 1, strlen(content), f);
    fclose(f);

    remove(full_path);
    if (rename(tmp_path, full_path) != 0) {
        gagent_error_init(err, GAGENT_ERR_FILESYSTEM, LINK_LOCAL_FILESYSTEM, LINK_ENGINE_RUNTIME,
                          -1, full_path, "Failed to rename temporary file to target");
        return -1;
    }
    return 0;
}

int tools_read_file_safe(LocalToolExecutor *tools, const char *rel_path,
                         char *out_buf, size_t out_max, GagentError *err) {
    if (!tools || !rel_path || !out_buf || out_max == 0) return -1;

    char full_path[GAGENT_MAX_PATH];
    if (boundary_resolve_safe_path(&tools->boundary, rel_path, full_path, sizeof(full_path), err) != 0) {
        return -1;
    }

    FILE *f = fopen(full_path, "rb");
    if (!f) {
        gagent_error_init(err, GAGENT_ERR_FILESYSTEM, LINK_LOCAL_FILESYSTEM, LINK_ENGINE_RUNTIME,
                          404, full_path, "File not found inside AllowedFolderBoundary");
        return -1;
    }
    size_t n = fread(out_buf, 1, out_max - 1, f);
    fclose(f);
    out_buf[n] = '\0';
    return 0;
}

int tools_rollback_file(LocalToolExecutor *tools, const SelectiveSession *sess,
                        const char *full_path, GagentError *err) {
    (void)tools;
    if (!sess || !full_path) return -1;

    const FileSnapshot *snap = session_get_snapshot(sess, full_path);
    if (!snap) {
        gagent_error_init(err, GAGENT_ERR_FILESYSTEM, LINK_LOCAL_FILESYSTEM, LINK_ENGINE_RUNTIME,
                          404, full_path, "No snapshot recorded in SelectiveSession for path");
        return -1;
    }

    if (snap->existed_before) {
        FILE *f = fopen(snap->path, "wb");
        if (!f) {
            gagent_error_init(err, GAGENT_ERR_FILESYSTEM, LINK_LOCAL_FILESYSTEM, LINK_ENGINE_RUNTIME,
                              -1, snap->path, "Rollback failed to open target file");
            return -1;
        }
        fwrite(snap->original_bytes, 1, snap->byte_len, f);
        fclose(f);
    } else {
        remove(snap->path);
    }
    return 0;
}
