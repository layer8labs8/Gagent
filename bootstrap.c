/*
 * G AGENT (v3.1) - AllowedFolderBoundary & Micro-Rootfs Bootstrapper
 * File: src/bootstrap.c
 */

#include "gagent.h"
#include <string.h>
#include <stdio.h>
#include <sys/stat.h>

#ifdef _WIN32
#include <direct.h>
#define MKDIR(p) _mkdir(p)
#define PATH_SEP '\\'
#else
#include <unistd.h>
#define MKDIR(p) mkdir(p, 0755)
#define PATH_SEP '/'
#endif

static int make_dir_if_missing(const char *path) {
    struct stat st;
    if (stat(path, &st) == 0) return 0;
    return MKDIR(path);
}

int boundary_init(AllowedFolderBoundary *b, const char *host_root) {
    if (!b || !host_root || !host_root[0]) return -1;
    memset(b, 0, sizeof(AllowedFolderBoundary));
    strncpy(b->host_root, host_root, GAGENT_MAX_PATH - 1);
    b->locked = 1;
    make_dir_if_missing(b->host_root);
    return 0;
}

int boundary_resolve_safe_path(const AllowedFolderBoundary *b, const char *rel_path,
                               char *out_full_path, size_t out_max, GagentError *err) {
    if (!b || !rel_path || !out_full_path || out_max == 0) {
        gagent_error_init(err, GAGENT_ERR_FILESYSTEM, LINK_LOCAL_FILESYSTEM, LINK_ENGINE_RUNTIME,
                          -1, "", "Invalid arguments to boundary_resolve_safe_path");
        return -1;
    }

    /* Block parent directory traversal ("..") and absolute root escapes */
    if (strstr(rel_path, "..") != NULL || rel_path[0] == '/' || rel_path[0] == '\\' ||
        strchr(rel_path, ':') != NULL) {
        gagent_error_init(err, GAGENT_ERR_PATH_TRAVERSAL, LINK_LOCAL_FILESYSTEM, LINK_ENGINE_RUNTIME,
                          403, rel_path, "Parent directory traversal ('..') or absolute path escape blocked by AllowedFolderBoundary");
        return -1;
    }

    snprintf(out_full_path, out_max, "%s%c%s", b->host_root, PATH_SEP, rel_path);
    return 0;
}

int bootstrap_sandbox_tree(const char *host_root, const char *sandbox_root, GagentError *err) {
    char sub[GAGENT_MAX_PATH];

    make_dir_if_missing(host_root);
    make_dir_if_missing(sandbox_root);

    snprintf(sub, sizeof(sub), "%s%crootfs", sandbox_root, PATH_SEP);
    make_dir_if_missing(sub);

    snprintf(sub, sizeof(sub), "%s%crootfs%cetc", sandbox_root, PATH_SEP, PATH_SEP);
    make_dir_if_missing(sub);

    snprintf(sub, sizeof(sub), "%s%crootfs%cworkspace", sandbox_root, PATH_SEP, PATH_SEP);
    make_dir_if_missing(sub);

    snprintf(sub, sizeof(sub), "%s%cvault", sandbox_root, PATH_SEP);
    make_dir_if_missing(sub);

    snprintf(sub, sizeof(sub), "%s%crootfs%cetc%cos-release", sandbox_root, PATH_SEP, PATH_SEP, PATH_SEP);
    FILE *f = fopen(sub, "wb");
    if (!f) {
        gagent_error_init(err, GAGENT_ERR_FILESYSTEM, LINK_PROCESS_SANDBOX, LINK_ENGINE_RUNTIME,
                          -1, sub, "Failed to provision .gagent_sandbox/rootfs/etc/os-release");
        return -1;
    }
    const char *os_info = "NAME=\"Ubuntu\"\nVERSION=\"24.04 LTS (G AGENT C Micro-Rootfs)\"\nID=ubuntu\n";
    fwrite(os_info, 1, strlen(os_info), f);
    fclose(f);
    return 0;
}
