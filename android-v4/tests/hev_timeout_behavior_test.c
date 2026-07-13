#include <arpa/inet.h>
#include <assert.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#include <hev-memory-allocator.h>
#include <hev-object.h>
#include <hev-socks5-client-tcp.h>
#include <hev-socks5-misc.h>
#include <hev-task.h>
#include <hev-task-io.h>
#include <hev-task-io-socket.h>
#include <hev-task-system.h>

#include "hev-config.h"
#include "hev-socks5-session.h"

enum
{
    FAST_TIMEOUT_MS = 80,
    STALLED_SERVER_MS = 640,
    DELAYED_DATA_MS = 240,
    CANCEL_DELAY_MS = 40,
};

typedef enum
{
    SERVER_STALL_HANDSHAKE,
    SERVER_DELAYED_DATA,
    SERVER_WAIT_FOR_CANCEL,
} ServerMode;

typedef struct
{
    int listener;
    unsigned short port;
    ServerMode mode;
    pthread_t thread;
} MockServer;

typedef struct _TestSession TestSession;
typedef struct _TestSessionClass TestSessionClass;

struct _TestSession
{
    HevSocks5ClientTCP base;
    HevSocks5SessionData data;
    volatile int splicer_entered;
    int splice_result;
    int splice_elapsed_ms;
};

struct _TestSessionClass
{
    HevSocks5ClientTCPClass base;
    HevSocks5SessionIface session;
};

typedef struct
{
    MockServer server;
    volatile TestSession *session;
    int session_elapsed_ms;
    int splicer_entered;
    int splice_result;
    int splice_elapsed_ms;
    int cancel_sent;
} Scenario;

static int64_t
monotonic_ms (void)
{
    struct timespec now;
    assert (clock_gettime (CLOCK_MONOTONIC, &now) == 0);
    return (int64_t)now.tv_sec * 1000 + now.tv_nsec / 1000000;
}

static void
sleep_ms (int delay_ms)
{
    struct timespec delay = {
        .tv_sec = delay_ms / 1000,
        .tv_nsec = (long)(delay_ms % 1000) * 1000000L,
    };
    while (nanosleep (&delay, &delay) != 0)
        ;
}

static void
read_exact (int fd, void *data, size_t size)
{
    unsigned char *cursor = data;
    while (size) {
        ssize_t count = recv (fd, cursor, size, 0);
        assert (count > 0);
        cursor += count;
        size -= count;
    }
}

static void
write_exact (int fd, const void *data, size_t size)
{
    const unsigned char *cursor = data;
    while (size) {
        ssize_t count = send (fd, cursor, size, 0);
        assert (count > 0);
        cursor += count;
        size -= count;
    }
}

static void
complete_socks_handshake (int fd)
{
    unsigned char auth[3];
    unsigned char request[10];
    static const unsigned char auth_reply[] = { 5, 0 };
    static const unsigned char connect_reply[] = {
        5, 0, 0, 1, 127, 0, 0, 1, 0, 0,
    };

    read_exact (fd, auth, sizeof (auth));
    assert (auth[0] == 5 && auth[1] == 1 && auth[2] == 0);
    write_exact (fd, auth_reply, sizeof (auth_reply));
    read_exact (fd, request, sizeof (request));
    assert (request[0] == 5 && request[1] == 1 && request[2] == 0 &&
            request[3] == 1);
    write_exact (fd, connect_reply, sizeof (connect_reply));
}

static void *
mock_server_entry (void *data)
{
    MockServer *server = data;
    int client = accept (server->listener, NULL, NULL);
    assert (client >= 0);

    if (server->mode == SERVER_STALL_HANDSHAKE) {
        unsigned char auth[3];
        read_exact (client, auth, sizeof (auth));
        sleep_ms (STALLED_SERVER_MS);
    } else {
        complete_socks_handshake (client);
        if (server->mode == SERVER_DELAYED_DATA) {
            static const char payload[] = "one-way";
            sleep_ms (DELAYED_DATA_MS);
            write_exact (client, payload, sizeof (payload) - 1);
        } else {
            char byte;
            assert (recv (client, &byte, 1, 0) == 0);
        }
    }

    close (client);
    close (server->listener);
    return NULL;
}

static void
mock_server_start (MockServer *server, ServerMode mode)
{
    struct sockaddr_in address = {
        .sin_family = AF_INET,
        .sin_addr.s_addr = htonl (INADDR_LOOPBACK),
    };
    socklen_t length = sizeof (address);
    int reuse = 1;

    memset (server, 0, sizeof (*server));
    server->mode = mode;
    server->listener = socket (AF_INET, SOCK_STREAM, 0);
    assert (server->listener >= 0);
    assert (setsockopt (server->listener, SOL_SOCKET, SO_REUSEADDR, &reuse,
                        sizeof (reuse)) == 0);
    assert (bind (server->listener, (struct sockaddr *)&address,
                  sizeof (address)) == 0);
    assert (getsockname (server->listener, (struct sockaddr *)&address,
                         &length) == 0);
    server->port = ntohs (address.sin_port);
    assert (listen (server->listener, 1) == 0);
    assert (pthread_create (&server->thread, NULL, mock_server_entry, server) ==
            0);
}

static void
mock_server_join (MockServer *server)
{
    assert (pthread_join (server->thread, NULL) == 0);
}

static void
test_session_splice (HevSocks5Session *base)
{
    TestSession *self = base;
    static const char expected[] = "one-way";
    char payload[sizeof (expected) - 1];
    int64_t started = monotonic_ms ();

    self->splicer_entered = 1;
    self->splice_result = hev_task_io_socket_recv (
        HEV_SOCKS5 (self)->fd, payload, sizeof (payload), MSG_WAITALL,
        hev_socks5_task_io_yielder, self);
    self->splice_elapsed_ms = monotonic_ms () - started;
    if (self->splice_result == (int)sizeof (payload))
        assert (memcmp (payload, expected, sizeof (payload)) == 0);
}

static HevTask *
test_session_get_task (HevSocks5Session *base)
{
    return ((TestSession *)base)->data.task;
}

static void
test_session_set_task (HevSocks5Session *base, HevTask *task)
{
    ((TestSession *)base)->data.task = task;
}

static HevListNode *
test_session_get_node (HevSocks5Session *base)
{
    return &((TestSession *)base)->data.node;
}

static void
test_session_destruct (HevObject *base)
{
    HEV_SOCKS5_CLIENT_TCP_TYPE->destruct (base);
}

static void *
test_session_iface (HevObject *base, void *type)
{
    TestSessionClass *klass = HEV_OBJECT_GET_CLASS (base);
    if (type == HEV_SOCKS5_SESSION_TYPE)
        return &klass->session;
    return HEV_SOCKS5_CLIENT_TCP_TYPE->iface (base, type);
}

static HevObjectClass *
test_session_class (void)
{
    static TestSessionClass klass;
    HevObjectClass *object = HEV_OBJECT_CLASS (&klass);

    if (!object->name) {
        memcpy (&klass, HEV_SOCKS5_CLIENT_TCP_TYPE,
                sizeof (HevSocks5ClientTCPClass));
        object->name = "TimeoutBehaviorTestSession";
        object->destruct = test_session_destruct;
        object->iface = test_session_iface;
        klass.session.splicer = test_session_splice;
        klass.session.get_task = test_session_get_task;
        klass.session.set_task = test_session_set_task;
        klass.session.get_node = test_session_get_node;
    }
    return object;
}

static TestSession *
test_session_new (void)
{
    TestSession *self;
    HevSocks5Addr target;
    uint32_t address = inet_addr ("198.51.100.10");

    self = hev_malloc0 (sizeof (*self));
    assert (self);
    assert (hev_socks5_addr_from_ipv4 (&target, &address, 443) > 0);
    assert (hev_socks5_client_tcp_construct (&self->base, &target) == 0);
    HEV_OBJECT (self)->klass = test_session_class ();
    self->data.self = self;
    self->splice_result = -999;
    return self;
}

static void
configure_session (unsigned short port)
{
    char config[512];
    int length = snprintf (
        config, sizeof (config),
        "tunnel:\n"
        "  mtu: 1500\n"
        "socks5:\n"
        "  address: '127.0.0.1'\n"
        "  port: %u\n"
        "  udp: 'tcp'\n"
        "misc:\n"
        "  connect-timeout: %d\n"
        "  tcp-read-write-timeout: 0\n"
        "  udp-read-write-timeout: 120000\n",
        port, FAST_TIMEOUT_MS);
    assert (length > 0 && length < (int)sizeof (config));
    assert (hev_config_init_from_str ((const unsigned char *)config, length) ==
            0);
    assert (hev_config_get_misc_tcp_read_write_timeout () == -1);
    hev_socks5_set_connect_timeout (FAST_TIMEOUT_MS);
    hev_socks5_set_tcp_timeout (FAST_TIMEOUT_MS);
    hev_socks5_set_udp_timeout (120000);
}

static void
session_task_entry (void *data)
{
    Scenario *scenario = data;
    TestSession *session = test_session_new ();
    int64_t started;

    scenario->session = session;
    hev_socks5_session_set_task (session, hev_task_self ());
    started = monotonic_ms ();
    hev_socks5_session_run (session);
    scenario->session_elapsed_ms = monotonic_ms () - started;
    scenario->splicer_entered = session->splicer_entered;
    scenario->splice_result = session->splice_result;
    scenario->splice_elapsed_ms = session->splice_elapsed_ms;
    scenario->session = NULL;
    hev_object_unref (HEV_OBJECT (session));
}

static void
cancel_task_entry (void *data)
{
    Scenario *scenario = data;
    TestSession *session;

    while (!(session = (TestSession *)scenario->session) ||
           !session->splicer_entered)
        hev_task_yield (HEV_TASK_YIELD);
    assert (hev_task_sleep (CANCEL_DELAY_MS) == 0);
    hev_socks5_session_terminate (session);
    scenario->cancel_sent = 1;
}

static void
run_scenario (Scenario *scenario, ServerMode mode, int cancel)
{
    HevTask *session_task;

    memset (scenario, 0, sizeof (*scenario));
    mock_server_start (&scenario->server, mode);
    configure_session (scenario->server.port);
    assert (hev_task_system_init () == 0);
    session_task = hev_task_new (-1);
    assert (session_task);
    hev_task_run (session_task, session_task_entry, scenario);
    if (cancel) {
        HevTask *cancel_task = hev_task_new (-1);
        assert (cancel_task);
        hev_task_run (cancel_task, cancel_task_entry, scenario);
    }
    hev_task_system_run ();
    hev_task_system_fini ();
    mock_server_join (&scenario->server);
}

int
main (void)
{
    Scenario scenario;

    signal (SIGPIPE, SIG_IGN);

    run_scenario (&scenario, SERVER_STALL_HANDSHAKE, 0);
    assert (!scenario.splicer_entered);
    assert (scenario.session_elapsed_ms >= FAST_TIMEOUT_MS / 2);
    assert (scenario.session_elapsed_ms < FAST_TIMEOUT_MS * 5);

    run_scenario (&scenario, SERVER_DELAYED_DATA, 0);
    assert (scenario.splicer_entered);
    assert (scenario.splice_result == (int)strlen ("one-way"));
    assert (scenario.splice_elapsed_ms >= DELAYED_DATA_MS - 40);
    assert (scenario.splice_elapsed_ms < DELAYED_DATA_MS * 3);

    run_scenario (&scenario, SERVER_WAIT_FOR_CANCEL, 1);
    assert (scenario.cancel_sent);
    assert (scenario.splicer_entered);
    assert (scenario.splice_result < 0);
    assert (scenario.splice_elapsed_ms >= CANCEL_DELAY_MS / 2);
    assert (scenario.splice_elapsed_ms < FAST_TIMEOUT_MS * 4);

    hev_config_fini ();
    return 0;
}
