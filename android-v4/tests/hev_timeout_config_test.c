#include <assert.h>
#include <string.h>

#include <lwip/tcp.h>

#include "hev-config.h"

_Static_assert (LWIP_WND_SCALE == 1, "TCP window scaling must be enabled");
_Static_assert (TCP_RCV_SCALE == 2, "TCP receive scale must remain bounded");
_Static_assert (TCP_MSS == 8191, "window math assumes the pinned HEV MSS");
_Static_assert (TCP_WND == 262112, "TCP receive window drifted");
_Static_assert (TCP_SND_BUF == 262112, "TCP send buffer drifted");
_Static_assert (TCP_SNDLOWAT == 32764, "TCP writable threshold drifted");
_Static_assert (TCP_SND_QUEUELEN == 4096, "TCP send queue bound drifted");
_Static_assert (PBUF_POOL_SIZE == 33, "TCP receive pool bound drifted");

int
main (void)
{
    static const unsigned char config[] =
        "tunnel:\n"
        "  mtu: 1500\n"
        "socks5:\n"
        "  address: '127.0.0.1'\n"
        "  port: 31416\n"
        "  udp-port: 31418\n"
        "  udp: 'tcp'\n"
        "misc:\n"
        "  tcp-buffer-size: 262112\n"
        "  max-session-count: 64\n"
        "  connect-timeout: 5000\n"
        "  tcp-read-write-timeout: 0\n"
        "  udp-read-write-timeout: 120000\n";

    assert (hev_config_init_from_str (config, strlen ((const char *)config)) ==
            0);
    assert (hev_config_get_misc_connect_timeout () == 5000);
    assert (hev_config_get_misc_tcp_read_write_timeout () == -1);
    assert (hev_config_get_misc_udp_read_write_timeout () == 120000);
    assert (hev_config_get_misc_tcp_buffer_size () == 262112);
    assert (hev_config_get_misc_max_session_count () == 64);
    assert (hev_config_get_misc_task_stack_size () == 282592);
    hev_config_fini ();

    return 0;
}
