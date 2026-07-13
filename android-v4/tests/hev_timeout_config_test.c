#include <assert.h>
#include <string.h>

#include <lwip/tcp.h>

#include "hev-config.h"

_Static_assert (LWIP_WND_SCALE == 0, "TCP window scaling must remain disabled");
_Static_assert (TCP_MSS == 8191, "window math assumes the pinned HEV MSS");
_Static_assert (TCP_WND == 65528, "TCP receive window drifted");
_Static_assert (TCP_SND_BUF == 65528, "TCP send buffer drifted");
_Static_assert (TCP_SNDLOWAT == 32764, "TCP writable threshold drifted");
_Static_assert (TCP_SND_QUEUELEN == 1024, "TCP send queue bound drifted");
_Static_assert (PBUF_POOL_SIZE == 32, "TCP receive pool bound drifted");

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
        "  task-stack-size: 65536\n"
        "  tcp-buffer-size: 65536\n"
        "  max-session-count: 256\n"
        "  connect-timeout: 5000\n"
        "  tcp-read-write-timeout: 0\n"
        "  udp-read-write-timeout: 120000\n";

    assert (hev_config_init_from_str (config, strlen ((const char *)config)) ==
            0);
    assert (hev_config_get_misc_connect_timeout () == 5000);
    assert (hev_config_get_misc_tcp_read_write_timeout () == -1);
    assert (hev_config_get_misc_udp_read_write_timeout () == 120000);
    assert (hev_config_get_misc_tcp_buffer_size () == 65528);
    assert (hev_config_get_misc_max_session_count () == 256);
    assert (hev_config_get_misc_task_stack_size () == 86008);
    hev_config_fini ();

    return 0;
}
