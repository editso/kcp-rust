#ifdef __cplusplus
extern "C" {
#endif

#include <ikcp.h>

void ikcp_set_minrto(ikcpcb *kcp, int minrto) { kcp->rx_minrto = minrto; }

IUINT32 ikcp_get_mss(ikcpcb *kcp) { return kcp->mss; }

#ifdef __cplusplus
}
#endif