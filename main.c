/*
 * G AGENT (v3.1) - Main Entry Point & Pure C Verification Test Suite
 * File: src/main.c
 *
 * Compiles cleanly with any standard C compiler (GCC, Clang, Mingw-w64, MSVC):
 *   gcc -O2 -Wall -Wextra src/error.c src/session.c ... -o gagent_test
 */

#include "gagent.h"
#include <stdio.h>
#include <string.h>
#include <assert.h>

static const char *SAMPLE_C_MODULE =
    "/* G AGENT Verified C Task (gagent_task.c) */\n"
    "#include <stdio.h>\n\n"
    "int count_primes(int limit) {\n"
    "    int count = 0;\n"
    "    for (int n = 2; n <= limit; ++n) {\n"
    "        int prime = 1;\n"
    "        for (int d = 2; d * d <= n; ++d) {\n"
    "            if (n % d == 0) { prime = 0; break; }\n"
    "        }\n"
    "        if (prime) ++count;\n"
    "    }\n"
    "    return count;\n"
    "}\n";

int main(void) {
    GagentEngine engine;
    GagentError err;
    char summary[512];
    char trace[1024];

    printf("=== G AGENT (%s) Pure C Runtime Self-Test ===\n", GAGENT_VERSION);

    /* 1. Initialize Engine, ShadowSandbox, API Key Vault & Drive Bridge */
    int rc = engine_init(&engine, "Gagent_Workspace", ".gagent_sandbox", &err);
    assert(rc == 0);
    printf("[PASS] 1. Engine & ShadowSandbox rootfs initialized.\n");

    /* 2. Verify Path Traversal Guard */
    char resolved[GAGENT_MAX_PATH];
    rc = boundary_resolve_safe_path(&engine.boundary, "../../etc/passwd", resolved, sizeof(resolved), &err);
    assert(rc != 0 && err.code == GAGENT_ERR_PATH_TRAVERSAL);
    printf("[PASS] 2. AllowedFolderBoundary blocked '../' path traversal.\n");

    /* 3. Verify Command Blocklist Policy */
    assert(tools_is_command_blocked("rm -rf /") == 1);
    assert(tools_is_command_blocked("gcc -O2 gagent_task.c") == 0);
    printf("[PASS] 3. LocalToolExecutor command blocklist verified.\n");

    /* 4. Execute Full Copy-Edit-Verify-Promote Pipeline in ShadowSandbox */
    rc = engine_execute_verified_task(&engine, "gagent_task.c", SAMPLE_C_MODULE, trace, sizeof(trace), &err);
    assert(rc == 0);
    printf("[PASS] 4. ShadowSandbox verified & promoted 'gagent_task.c':\n%s\n", trace);

    /* 5. Test Atomic Write & SelectiveSession Snapshot Rollback */
    rc = tools_write_file_atomic(&engine.tools, &engine.session, "config.h", "#define GAGENT_MODE 1\n", &err);
    assert(rc == 0);

    rc = tools_write_file_atomic(&engine.tools, &engine.session, "config.h", "CORRUPTED_DATA", &err);
    assert(rc == 0);

    char full_cfg[GAGENT_MAX_PATH];
    boundary_resolve_safe_path(&engine.boundary, "config.h", full_cfg, sizeof(full_cfg), &err);
    rc = tools_rollback_file(&engine.tools, &engine.session, full_cfg, &err);
    assert(rc == 0);

    char restored[256];
    rc = tools_read_file_safe(&engine.tools, "config.h", restored, sizeof(restored), &err);
    assert(rc == 0 && strcmp(restored, "#define GAGENT_MODE 1\n") == 0);
    printf("[PASS] 5. SelectiveSession atomic snapshot rollback verified.\n");

    /* 6. Run End-to-End Self-Diagnostics */
    rc = engine_run_self_diagnostics(&engine, summary, sizeof(summary), &err);
    assert(rc == 0);
    printf("[PASS] 6. %s\n", summary);
    printf("=== ALL 6 PURE C RUNTIME TESTS PASSED (EXIT 0) ===\n");
    return 0;
}
