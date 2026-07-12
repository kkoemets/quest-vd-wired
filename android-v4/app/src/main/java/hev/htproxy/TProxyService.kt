package hev.htproxy

class TProxyService {
    external fun TProxyStartService(configPath: String, tunFd: Int): Long

    external fun TProxyAwaitReady(token: Long, timeoutMs: Int): Int

    external fun TProxyStopService(token: Long): Boolean

    external fun TProxyAwaitStopped(token: Long, timeoutMs: Int): Int

    external fun TProxyGetStats(): LongArray

    companion object {
        init {
            System.loadLibrary("hev-socks5-tunnel")
        }
    }
}
