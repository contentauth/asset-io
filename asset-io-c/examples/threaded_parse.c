#define _POSIX_C_SOURCE 200809L
#include "asset_io.h"
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

typedef struct {
    pthread_mutex_t mutex;
    pthread_cond_t changed;
    int fd;
    int pending;
    int complete;
    int stop;
    AssetIoParseTask *task;
    AssetIoReadRequest request;
    size_t reads;
    pthread_t driver;
} Reader;

static void check_thread(int error) {
    if (error != 0) {
        fprintf(stderr, "pthread: %s\n", strerror(error));
        exit(1);
    }
}

static void library_error(void) {
    AssetIoBuffer *error = asset_io_last_error();
    if (error) {
        size_t len = 0;
        const unsigned char *bytes = asset_io_buffer_data(error, &len);
        if (bytes) {
            fwrite(bytes, 1, len, stderr);
            fputc('\n', stderr);
        }
        assert(asset_io_free(error) == 0);
    }
    exit(1);
}

static void *read_worker(void *context) {
    Reader *r = context;
    assert(!pthread_equal(pthread_self(), r->driver));
    check_thread(pthread_mutex_lock(&r->mutex));
    for (;;) {
        while (!r->pending && !r->stop)
            check_thread(pthread_cond_wait(&r->changed, &r->mutex));
        if (r->stop)
            break;
        AssetIoReadRequest request = r->request;
        check_thread(pthread_mutex_unlock(&r->mutex));

        unsigned char *bytes = malloc(request.window_len);
        size_t len = 0;
        int error = bytes ? 0 : ENOMEM;
        while (!error && len < request.window_len) {
            uint64_t offset = request.offset + len;
            if (offset > INT64_MAX) {
                error = EOVERFLOW;
                break;
            }
            ssize_t n = pread(r->fd, bytes + len, request.window_len - len,
                              (off_t)offset);
            if (n < 0) {
                if (errno == EINTR)
                    continue;
                error = errno;
                break;
            }
            if (n == 0)
                break;
            len += (size_t)n;
        }

        int supplied = error
            ? asset_io_parse_task_fail(r->task, strerror(error))
            : asset_io_parse_task_supply(r->task, bytes, len);
        free(bytes);
        if (supplied != 0)
            library_error();
        check_thread(pthread_mutex_lock(&r->mutex));
        r->pending = 0;
        r->complete = 1;
        r->reads++;
        check_thread(pthread_cond_broadcast(&r->changed));
    }
    check_thread(pthread_mutex_unlock(&r->mutex));
    return NULL;
}

int main(int argc, char **argv) {
    if (argc < 2 || argc > 3) {
        fprintf(stderr, "usage: %s FILE [FORMAT]\n", argv[0]);
        return 2;
    }
    int fd = open(argv[1], O_RDONLY);
    if (fd < 0) {
        perror("open");
        return 1;
    }
    struct stat st;
    if (fstat(fd, &st) != 0) {
        perror("fstat");
        close(fd);
        return 1;
    }
    if (st.st_size < 0) {
        fprintf(stderr, "negative source size\n");
        close(fd);
        return 1;
    }
    const char *format = argc == 3 ? argv[2] : NULL;
    AssetIoRegistry *registry = asset_io_registry_new(1);
    if (!registry)
        library_error();
    AssetIoParseTask *task = asset_io_parse_task_new(registry, format, (uint64_t)st.st_size);
    if (!task)
        library_error();
    Reader reader = {
        .mutex = PTHREAD_MUTEX_INITIALIZER,
        .changed = PTHREAD_COND_INITIALIZER,
        .fd = fd,
        .task = task,
        .driver = pthread_self(),
    };
    pthread_t worker;
    check_thread(pthread_create(&worker, NULL, read_worker, &reader));
    int status;
    AssetIoReadRequest request;
    while ((status = asset_io_parse_task_resume(task, &request)) == 1) {
        assert(request.min_len > 0 && request.min_len <= request.window_len);
        assert(request.offset <= (uint64_t)st.st_size);
        assert(request.window_len <= (uint64_t)st.st_size - request.offset);
        check_thread(pthread_mutex_lock(&reader.mutex));
        reader.request = request;
        reader.complete = 0;
        reader.pending = 1;
        check_thread(pthread_cond_broadcast(&reader.changed));
        /* Rust has returned; only the native host waits for its worker. */
        while (!reader.complete)
            check_thread(pthread_cond_wait(&reader.changed, &reader.mutex));
        check_thread(pthread_mutex_unlock(&reader.mutex));
    }
    check_thread(pthread_mutex_lock(&reader.mutex));
    reader.stop = 1;
    check_thread(pthread_cond_broadcast(&reader.changed));
    check_thread(pthread_mutex_unlock(&reader.mutex));
    check_thread(pthread_join(worker, NULL));
    check_thread(pthread_cond_destroy(&reader.changed));
    check_thread(pthread_mutex_destroy(&reader.mutex));
    if (status != 0)
        library_error();
    assert(reader.reads > 0);

    AssetIoStructure *structure = asset_io_parse_task_structure(task);
    if (!structure)
        library_error();
    assert(asset_io_free(task) == 0);
    AssetIoBuffer *actual = asset_io_structure_json(structure);
    if (!actual)
        library_error();

    unsigned char *input = malloc(st.st_size ? (size_t)st.st_size : 1);
    if (!input) {
        perror("malloc");
        return 1;
    }
    size_t len = 0;
    while (len < (size_t)st.st_size) {
        ssize_t n = read(fd, input + len, (size_t)st.st_size - len);
        if (n < 0 && errno == EINTR)
            continue;
        if (n <= 0) {
            fprintf(stderr, "blocking comparison read failed\n");
            return 1;
        }
        len += (size_t)n;
    }
    AssetIoAsset *blocking = asset_io_parse_memory(registry, format, input, len);
    free(input);
    if (!blocking)
        library_error();
    AssetIoStructure *expected_structure = asset_io_asset_structure(blocking);
    if (!expected_structure)
        library_error();
    AssetIoBuffer *expected = asset_io_structure_json(expected_structure);
    if (!expected)
        library_error();
    size_t actual_len = 0, expected_len = 0;
    const unsigned char *actual_bytes = asset_io_buffer_data(actual, &actual_len);
    const unsigned char *expected_bytes = asset_io_buffer_data(expected, &expected_len);
    assert(actual_bytes && expected_bytes);
    assert(actual_len == expected_len);
    assert(memcmp(actual_bytes, expected_bytes, actual_len) == 0);
    printf("PASS: %zu native-thread reads; structure matches blocking parsing\n",
           reader.reads);
    assert(asset_io_free(expected) == 0);
    assert(asset_io_free(expected_structure) == 0);
    assert(asset_io_free(blocking) == 0);
    assert(asset_io_free(actual) == 0);
    assert(asset_io_free(structure) == 0);
    assert(asset_io_free(registry) == 0);
    if (close(fd) != 0) {
        perror("close");
        return 1;
    }
    return 0;
}
