/* Development-only, private-free macOS descriptor snapshot. */
#include <errno.h>
#include <inttypes.h>
#include <limits.h>
#include <libproc.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/proc_info.h>
#include <unistd.h>

#define MAX_ENTRIES ((size_t)131072)
#define MAX_ATTEMPTS 3

#ifdef AUTOROUTER_FD_TEST
#include <signal.h>
static const char *scenario;
static unsigned identity_calls, list_calls;
static int probe(int pid, int flavor, void *buffer, int length) {
    if (strcmp(scenario, "real") == 0) return proc_pidinfo(pid, flavor, 0, buffer, length);
    if (strcmp(scenario, "blocked") == 0) {
        if (write(STDOUT_FILENO, "blocked\n", 8) != 8) _exit(2);
        for (;;) pause();
    }
    if (flavor == PROC_PIDTBSDINFO) {
        struct proc_bsdinfo *info = buffer;
        memset(info, 0, sizeof(*info));
        info->pbi_pid = (uint32_t)pid;
        info->pbi_start_tvsec = 23;
        info->pbi_start_tvusec = 41;
        if (strcmp(scenario, "identity_changed") == 0 && identity_calls != 0) info->pbi_start_tvusec++;
        identity_calls++;
        return (int)sizeof(*info);
    }
    if (buffer == NULL) {
        if (strcmp(scenario, "overflow_hint") == 0) return (int)(131073 * sizeof(struct proc_fdinfo));
        if (strcmp(scenario, "integer_overflow_hint") == 0) return INT_MAX;
        if (strcmp(scenario, "misaligned_hint") == 0) return (int)sizeof(struct proc_fdinfo) + 1;
        return (int)sizeof(struct proc_fdinfo);
    }
    list_calls++;
    if (strcmp(scenario, "reported_overflow") == 0) return length + 1;
    if (strcmp(scenario, "misaligned_records") == 0) return (int)sizeof(struct proc_fdinfo) + 1;
    if ((strcmp(scenario, "unstable_growth") == 0 && list_calls <= 3) || (strcmp(scenario, "growth_then_stable") == 0 && list_calls == 1)) return length;
    struct proc_fdinfo *entries = buffer;
    entries[0].proc_fd = 0;
    entries[1].proc_fd = 3;
    if (strcmp(scenario, "duplicate") == 0) entries[1].proc_fd = 0;
    if (strcmp(scenario, "negative") == 0) entries[1].proc_fd = -1;
    return (int)(2 * sizeof(*entries));
}
static int target(void) {
    int pipes[2] = {-1, -1};
    puts("ready"); fflush(stdout);
    for (int command; (command = getchar()) != EOF;) {
        if (command == 'o' && pipes[0] == -1 && pipe(pipes) == 0) puts("opened");
        else if (command == 'c' && pipes[0] != -1) {
            close(pipes[0]); close(pipes[1]); pipes[0] = pipes[1] = -1; puts("closed");
        } else if (command == 'q') break;
        else return 2;
        fflush(stdout);
    }
    if (pipes[0] != -1) { close(pipes[0]); close(pipes[1]); }
    return 0;
}
#else
static int probe(int pid, int flavor, void *buffer, int length) {
    return proc_pidinfo(pid, flavor, 0, buffer, length);
}
#endif

static int emit(const char *status, int verified, size_t count) {
    if (verified) printf("{\"schema_version\":1,\"method\":\"libproc_pid_numeric_records_v1\",\"status\":\"%s\",\"verified\":true,\"count\":%zu}\n", status, count);
    else printf("{\"schema_version\":1,\"method\":\"libproc_pid_numeric_records_v1\",\"status\":\"%s\",\"verified\":false,\"count\":null}\n", status);
    return fflush(stdout) == 0 ? 0 : 1;
}
static int compare_fd(const void *left, const void *right) {
    int32_t a = ((const struct proc_fdinfo *)left)->proc_fd;
    int32_t b = ((const struct proc_fdinfo *)right)->proc_fd;
    return (a > b) - (a < b);
}
static int identity(int pid, struct proc_bsdinfo *info) {
    memset(info, 0, sizeof(*info));
    return probe(pid, PROC_PIDTBSDINFO, info, (int)sizeof(*info)) == (int)sizeof(*info)
        && info->pbi_pid == (uint32_t)pid;
}
int main(int argc, char **argv) {
#ifdef AUTOROUTER_FD_TEST
    if (argc == 2 && strcmp(argv[1], "--target") == 0) return target();
    if (argc != 3) return emit("invalid_arguments", 0, 0);
    scenario = argv[2];
#else
    if (argc != 2) return emit("invalid_arguments", 0, 0);
#endif
    if (*argv[1] == '\0') return emit("invalid_pid", 0, 0);
    for (const char *p = argv[1]; *p; p++) if (*p < '0' || *p > '9') return emit("invalid_pid", 0, 0);
    errno = 0;
    char *end;
    unsigned long parsed = strtoul(argv[1], &end, 10);
    if (errno != 0 || *end != '\0' || parsed == 0 || parsed > INT_MAX) return emit("invalid_pid", 0, 0);
    int pid = (int)parsed;
    if (sizeof(struct proc_fdinfo) > SIZE_MAX / MAX_ENTRIES || MAX_ENTRIES * sizeof(struct proc_fdinfo) > INT_MAX) return emit("sdk_size_overflow", 0, 0);
    size_t max_bytes = MAX_ENTRIES * sizeof(struct proc_fdinfo);
    struct proc_bsdinfo before, after;
    if (!identity(pid, &before)) return emit("process_unavailable", 0, 0);
    int hint = probe(pid, PROC_PIDLISTFDS, NULL, 0);
    if (hint <= 0) return emit("list_unavailable", 0, 0);
    if ((size_t)hint > max_bytes) return emit("entry_limit", 0, 0);
    if ((size_t)hint % sizeof(struct proc_fdinfo) != 0) return emit("misaligned_hint", 0, 0);
    size_t capacity = (size_t)hint / sizeof(struct proc_fdinfo);
    capacity = capacity > MAX_ENTRIES - 16 ? MAX_ENTRIES : capacity + 16;
    const char *failure = "enumeration_unstable";
    for (unsigned attempt = 0; attempt < MAX_ATTEMPTS; attempt++) {
        size_t allocated = capacity * sizeof(struct proc_fdinfo);
        struct proc_fdinfo *entries = malloc(allocated);
        if (entries == NULL) return emit("allocation_failed", 0, 0);
        int length = probe(pid, PROC_PIDLISTFDS, entries, (int)allocated);
        if (length <= 0) failure = "list_unavailable";
        else if ((size_t)length > allocated) failure = "reported_overflow";
        else if ((size_t)length % sizeof(*entries) != 0) failure = "misaligned_records";
        else if ((size_t)length == allocated) {
            free(entries);
            if (capacity == MAX_ENTRIES || attempt + 1 == MAX_ATTEMPTS) return emit("enumeration_unstable", 0, 0);
            capacity = capacity > MAX_ENTRIES / 2 ? MAX_ENTRIES : capacity * 2;
            continue;
        } else {
            size_t count = (size_t)length / sizeof(*entries);
            qsort(entries, count, sizeof(*entries), compare_fd);
            int valid = 1;
            for (size_t index = 0; index < count; index++) {
                if (entries[index].proc_fd < 0 || (index != 0 && entries[index - 1].proc_fd == entries[index].proc_fd)) valid = 0;
            }
            free(entries);
            if (!valid) return emit("invalid_records", 0, 0);
            if (!identity(pid, &after)) return emit("process_unavailable", 0, 0);
            if (before.pbi_pid != after.pbi_pid || before.pbi_start_tvsec != after.pbi_start_tvsec || before.pbi_start_tvusec != after.pbi_start_tvusec) return emit("process_identity_changed", 0, 0);
            return emit("collected", 1, count);
        }
        free(entries);
        return emit(failure, 0, 0);
    }
    return emit(failure, 0, 0);
}
