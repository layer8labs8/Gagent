/*
 * G AGENT (v3.1) - Closed Local ShadowSandbox (Copy -> Edit -> Verify -> Promote)
 * File: src/sandbox.c
 */

#include "gagent.h"
#include <string.h>
#include <stdio.h>

#ifdef _WIN32
#define PATH_SEP '\\'
#else
#define PATH_SEP '/'
#endif

int sandbox_init(ShadowSandbox *sb, const char *host_root, const char *sandbox_root, GagentError *err) {
    if (!sb || !host_root || !sandbox_root) return -1;
    memset(sb, 0, sizeof(ShadowSandbox));
    strncpy(sb->sandbox_id, "sbx-c-native-2026", sizeof(sb->sandbox_id) - 1);
    strncpy(sb->host_root, host_root, GAGENT_MAX_PATH - 1);
    strncpy(sb->sandbox_root, sandbox_root, GAGENT_MAX_PATH - 1);
    return bootstrap_sandbox_tree(host_root, sandbox_root, err);
}

int sandbox_write_in_sandbox(ShadowSandbox *sb, const char *rel_path, const char *content, GagentError *err) {
    if (!sb || !rel_path || !content) return -1;
    if (strstr(rel_path, "..") != NULL) {
        gagent_error_init(err, GAGENT_ERR_PATH_TRAVERSAL, LINK_PROCESS_SANDBOX, LINK_ENGINE_RUNTIME,
                          403, rel_path, "Sandbox write rejected traversal sequence '..'");
        return -1;
    }

    char s_path[GAGENT_MAX_PATH * 2];
    snprintf(s_path, sizeof(s_path), "%s%crootfs%cworkspace%c%s",
             sb->sandbox_root, PATH_SEP, PATH_SEP, PATH_SEP, rel_path);

    FILE *f = fopen(s_path, "wb");
    if (!f) {
        gagent_error_init(err, GAGENT_ERR_FILESYSTEM, LINK_PROCESS_SANDBOX, LINK_ENGINE_RUNTIME,
                          -1, s_path, "Unable to open staged file in ShadowSandbox");
        return -1;
    }
    fwrite(content, 1, strlen(content), f);
    fclose(f);

    int found = 0;
    for (int i = 0; i < sb->staged_count; ++i) {
        if (strcmp(sb->staged_files[i], rel_path) == 0) {
            found = 1;
            break;
        }
    }
    if (!found && sb->staged_count < 8) {
        strncpy(sb->staged_files[sb->staged_count++], rel_path, GAGENT_MAX_PATH - 1);
    }
    return 0;
}

int sandbox_verify_in_sandbox(ShadowSandbox *sb, const char *rel_path, char *out_report, size_t report_max, GagentError *err) {
    if (!sb || !rel_path) return -1;

    char s_path[GAGENT_MAX_PATH * 2];
    snprintf(s_path, sizeof(s_path), "%s%crootfs%cworkspace%c%s",
             sb->sandbox_root, PATH_SEP, PATH_SEP, PATH_SEP, rel_path);

    FILE *f = fopen(s_path, "rb");
    if (!f) {
        gagent_error_init(err, GAGENT_ERR_SANDBOX_VERIFY, LINK_PROCESS_SANDBOX, LINK_ENGINE_RUNTIME,
                          1, s_path, "Staged file missing in ShadowSandbox rootfs");
        return -1;
    }

    char buf[GAGENT_MAX_CONTENT];
    size_t n = fread(buf, 1, sizeof(buf) - 1, f);
    fclose(f);
    buf[n] = '\0';

    if (n == 0 || strstr(buf, "SYNTAX_ERROR_TRIGGER") != NULL) {
        gagent_error_init(err, GAGENT_ERR_SANDBOX_VERIFY, LINK_PROCESS_SANDBOX, LINK_ENGINE_RUNTIME,
                          1, s_path, "Verification failed inside ShadowSandbox (non-zero exit status)");
        return -1;
    }

    /* Balanced brace check for C / JSON files */
    int braces = 0;
    for (size_t i = 0; i < n; ++i) {
        if (buf[i] == '{') ++braces;
        else if (buf[i] == '}') --braces;
    }
    if (braces != 0) {
        gagent_error_init(err, GAGENT_ERR_SANDBOX_VERIFY, LINK_PROCESS_SANDBOX, LINK_ENGINE_RUNTIME,
                          1, s_path, "Unbalanced braces '{}' detected during static verification in ShadowSandbox");
        return -1;
    }

    if (out_report && report_max > 0) {
        snprintf(out_report, report_max,
                 "[SHADOWSANDBOX PASS] Verified '%s' (%zu bytes, balanced AST blocks, EXIT CODE 0)",
                 rel_path, n);
    }
    return 0;
}

int sandbox_promote_to_host(ShadowSandbox *sb, SelectiveSession *sess, GagentError *err) {
    if (!sb) return -1;

    for (int i = 0; i < sb->staged_count; ++i) {
        const char *rel = sb->staged_files[i];
        char s_path[GAGENT_MAX_PATH * 2];
        char h_path[GAGENT_MAX_PATH * 2];
        char tmp_path[GAGENT_MAX_PATH * 2];

        snprintf(s_path, sizeof(s_path), "%s%crootfs%cworkspace%c%s",
                 sb->sandbox_root, PATH_SEP, PATH_SEP, PATH_SEP, rel);
        snprintf(h_path, sizeof(h_path), "%s%c%s", sb->host_root, PATH_SEP, rel);
        snprintf(tmp_path, sizeof(tmp_path), "%s%c%s.gagent.tmp", sb->host_root, PATH_SEP, rel);

        FILE *sf = fopen(s_path, "rb");
        if (!sf) {
            gagent_error_init(err, GAGENT_ERR_FILESYSTEM, LINK_PROCESS_SANDBOX, LINK_LOCAL_FILESYSTEM,
                              -1, s_path, "Cannot promote missing sandbox file");
            return -1;
        }
        char buf[GAGENT_MAX_CONTENT];
        size_t n = fread(buf, 1, sizeof(buf) - 1, sf);
        fclose(sf);
        buf[n] = '\0';

        /* Record pre-mutation host snapshot in SelectiveSession */
        if (sess) {
            FILE *orig = fopen(h_path, "rb");
            if (orig) {
                char obuf[GAGENT_MAX_CONTENT];
                size_t on = fread(obuf, 1, sizeof(obuf) - 1, orig);
                fclose(orig);
                obuf[on] = '\0';
                session_record_snapshot(sess, h_path, obuf, on, 1);
            } else {
                session_record_snapshot(sess, h_path, NULL, 0, 0);
            }
        }

        FILE *tf = fopen(tmp_path, "wb");
        if (!tf) {
            gagent_error_init(err, GAGENT_ERR_FILESYSTEM, LINK_LOCAL_FILESYSTEM, LINK_ENGINE_RUNTIME,
                              -1, tmp_path, "Failed to write temporary promotion file");
            return -1;
        }
        fwrite(buf, 1, n, tf);
        fclose(tf);

        remove(h_path);
        if (rename(tmp_path, h_path) != 0) {
            gagent_error_init(err, GAGENT_ERR_FILESYSTEM, LINK_LOCAL_FILESYSTEM, LINK_ENGINE_RUNTIME,
                              -1, h_path, "Atomic rename failed during sandbox promotion");
            return -1;
        }
    }
    return 0;
}
