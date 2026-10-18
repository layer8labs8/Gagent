/*
 * G AGENT (v3.1) - Pure C Native Autonomous Desktop Runtime
 * File: src/gagent.h
 *
 * Complete C99/Win64 header defining all structures, fault taxonomies,
 * SelectiveSession snapshot rollback, AllowedFolderBoundary path guard,
 * ShadowSandbox copy-verify-promote pipeline, Gemini API Key Vault,
 * and Google Drive Zero-Disk Cloud Bridge.
 */

#ifndef GAGENT_H
#define GAGENT_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

#define GAGENT_VERSION          "3.1.0-native-c"
#define GAGENT_MAX_PATH         260
#define GAGENT_MAX_MSG          512
#define GAGENT_MAX_CONTENT      8192
#define GAGENT_MAX_SNAPSHOTS    8
#define GAGENT_MAX_HEAL_LOGS    16
#define GAGENT_SUBSYSTEM_COUNT  6

/* 1. Subsystem Link Identifiers */
typedef enum {
    LINK_AUTH_LOOPBACK    = 0,
    LINK_TOKEN_VAULT      = 1,
    LINK_WORKSPACE_BRIDGE = 2,
    LINK_LOCAL_FILESYSTEM = 3,
    LINK_PROCESS_SANDBOX  = 4,
    LINK_ENGINE_RUNTIME   = 5
} SubsystemLink;

/* 2. Deterministic Recovery Strategies for SelectiveSession */
typedef enum {
    STRATEGY_NONE                  = 0,
    STRATEGY_RELOAD_API_KEY        = 1,
    STRATEGY_REBIND_BRIDGE         = 2,
    STRATEGY_ROLLBACK_SNAPSHOT     = 3,
    STRATEGY_SANITIZE_AND_RETRY    = 4,
    STRATEGY_BACKOFF_AND_RETRY     = 5,
    STRATEGY_ESCALATE_TO_USER      = 6
} RecoveryStrategy;

/* 3. Unified Error Codes */
typedef enum {
    GAGENT_OK                      = 0,
    GAGENT_ERR_AUTH                = 1,
    GAGENT_ERR_LINKAGE             = 2,
    GAGENT_ERR_PROCESS             = 3,
    GAGENT_ERR_WORKSPACE_API       = 4,
    GAGENT_ERR_FILESYSTEM          = 5,
    GAGENT_ERR_PATH_TRAVERSAL      = 6,
    GAGENT_ERR_SANDBOX_VERIFY      = 7,
    GAGENT_ERR_SESSION_ABORT       = 8
} GagentErrorCode;

/* 4. Structured Error & Diagnostic Descriptor */
typedef struct {
    GagentErrorCode  code;
    SubsystemLink    origin;
    SubsystemLink    target;
    int              http_or_exit_status;
    int              recoverable;
    RecoveryStrategy suggested_action;
    char             target_path[GAGENT_MAX_PATH];
    char             message[GAGENT_MAX_MSG];
} GagentError;

/* 5.SelectiveSession Lifecycle Status */
typedef enum {
    SESSION_HEALTHY         = 0,
    SESSION_ISOLATING_FAULT = 1,
    SESSION_APPLYING_REMEDY = 2,
    SESSION_VERIFYING_LINK  = 3,
    SESSION_HEALED          = 4,
    SESSION_ESCALATED       = 5
} SelectiveSessionStatus;

/* 6. Pre-Mutation File Snapshot for Atomic Rollback */
typedef struct {
    int  in_use;
    int  existed_before;
    char path[GAGENT_MAX_PATH];
    char original_bytes[GAGENT_MAX_CONTENT];
    size_t byte_len;
} FileSnapshot;

/* 7. Self-Healing Audit Log Entry */
typedef struct {
    char             session_id[64];
    GagentErrorCode  fault_code;
    SubsystemLink    origin;
    RecoveryStrategy strategy_applied;
    int              resolved;
    int              attempt_count;
    char             summary[GAGENT_MAX_MSG];
} HealingRecord;

/* 8. SelectiveSession Context */
typedef struct {
    char                   id[64];
    SelectiveSessionStatus status;
    int                    max_attempts;
    int                    link_health[GAGENT_SUBSYSTEM_COUNT];
    FileSnapshot           snapshots[GAGENT_MAX_SNAPSHOTS];
    HealingRecord          healing_log[GAGENT_MAX_HEAL_LOGS];
    int                    healing_log_count;
} SelectiveSession;

/* 9. AllowedFolderBoundary (Strict Host Directory Containment) */
typedef struct {
    char host_root[GAGENT_MAX_PATH];
    int  locked;
} AllowedFolderBoundary;

/* 10. Closed Local ShadowSandbox */
typedef struct {
    char sandbox_id[64];
    char host_root[GAGENT_MAX_PATH];
    char sandbox_root[GAGENT_MAX_PATH];
    char staged_files[8][GAGENT_MAX_PATH];
    int  staged_count;
} ShadowSandbox;

/* 11. Direct Gemini API Key Vault (Zero-Client-ID Mode) */
typedef struct {
    char api_key[256];
    char active_model[64];
    char vault_path[GAGENT_MAX_PATH];
    int  is_linked;
} GeminiApiVault;

/* 12. Google Drive Zero-Disk Cloud Bridge */
typedef struct {
    char folder_id[128];
    char manifest_path[GAGENT_MAX_PATH];
    int  zero_disk_enabled;
    int  synced;
} DriveZeroDiskBridge;

/* 13. Local Tool Executor */
typedef struct {
    AllowedFolderBoundary boundary;
    int                   command_timeout_ms;
} LocalToolExecutor;

/* 14. Master G AGENT Runtime Engine */
typedef struct {
    SelectiveSession      session;
    AllowedFolderBoundary boundary;
    ShadowSandbox         sandbox;
    GeminiApiVault        auth;
    DriveZeroDiskBridge   drive;
    LocalToolExecutor     tools;
} GagentEngine;

/* Subsystem Function Prototypes */

/* src/error.c */
const char* gagent_link_name(SubsystemLink link);
const char* gagent_strategy_name(RecoveryStrategy strategy);
void gagent_error_clear(GagentError *err);
void gagent_error_init(GagentError *err, GagentErrorCode code, SubsystemLink origin,
                       SubsystemLink target, int status, const char *path, const char *msg);
int  gagent_error_is_recoverable(const GagentError *err);

/* src/session.c */
void session_init(SelectiveSession *sess, int max_attempts);
int  session_all_links_healthy(const SelectiveSession *sess);
void session_set_link_health(SelectiveSession *sess, SubsystemLink link, int healthy);
int  session_record_snapshot(SelectiveSession *sess, const char *path, const char *bytes, size_t len, int existed);
const FileSnapshot* session_get_snapshot(const SelectiveSession *sess, const char *path);
int  session_heal_fault(SelectiveSession *sess, const GagentError *fault);

/* src/bootstrap.c */
int  boundary_init(AllowedFolderBoundary *b, const char *host_root);
int  boundary_resolve_safe_path(const AllowedFolderBoundary *b, const char *rel_path,
                                char *out_full_path, size_t out_max, GagentError *err);
int  bootstrap_sandbox_tree(const char *host_root, const char *sandbox_root, GagentError *err);

/* src/sandbox.c */
int  sandbox_init(ShadowSandbox *sb, const char *host_root, const char *sandbox_root, GagentError *err);
int  sandbox_write_in_sandbox(ShadowSandbox *sb, const char *rel_path, const char *content, GagentError *err);
int  sandbox_verify_in_sandbox(ShadowSandbox *sb, const char *rel_path, char *out_report, size_t report_max, GagentError *err);
int  sandbox_promote_to_host(ShadowSandbox *sb, SelectiveSession *sess, GagentError *err);

/* src/auth.c */
void auth_vault_init(GeminiApiVault *vault, const char *vault_path);
int  auth_link_api_key(GeminiApiVault *vault, const char *raw_key, GagentError *err);
int  auth_verify_key(const GeminiApiVault *vault, GagentError *err);

/* src/workspace.c */
void workspace_bridge_init(DriveZeroDiskBridge *bridge, const char *folder_id, const char *manifest_path);
int  workspace_sync_zero_disk(DriveZeroDiskBridge *bridge, const char *artifact_name, size_t byte_size, GagentError *err);

/* src/tools.c */
void tools_init(LocalToolExecutor *tools, const char *host_root);
int  tools_is_command_blocked(const char *cmd_line);
int  tools_write_file_atomic(LocalToolExecutor *tools, SelectiveSession *sess,
                             const char *rel_path, const char *content, GagentError *err);
int  tools_read_file_safe(LocalToolExecutor *tools, const char *rel_path,
                          char *out_buf, size_t out_max, GagentError *err);
int  tools_rollback_file(LocalToolExecutor *tools, const SelectiveSession *sess,
                         const char *full_path, GagentError *err);

/* src/engine.c */
int  engine_init(GagentEngine *eng, const char *host_root, const char *sandbox_root, GagentError *err);
int  engine_run_self_diagnostics(GagentEngine *eng, char *out_summary, size_t max_len, GagentError *err);
int  engine_execute_verified_task(GagentEngine *eng, const char *rel_path, const char *code_content,
                                  char *out_trace, size_t trace_max, GagentError *err);

#ifdef __cplusplus
}
#endif

#endif /* GAGENT_H */
