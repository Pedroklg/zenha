locals {
  base_ports = [
    {
      name        = "AllowHttpInbound"
      protocol    = "tcp"
      port        = 80
      priority    = 100
      source_cidr = null
      description = "HTTP for ACME HTTP-01 challenges and HTTPS redirect (Caddy)"
    },
    {
      name        = "AllowHttpsInbound"
      protocol    = "tcp"
      port        = 443
      priority    = 110
      source_cidr = null
      description = "HTTPS for token server and LiveKit WSS signaling (Caddy)"
    },
    {
      name        = "AllowLiveKitRtcTcpInbound"
      protocol    = "tcp"
      port        = 7881
      priority    = 120
      source_cidr = null
      description = "LiveKit WebRTC ICE/TCP fallback"
    },
    {
      name        = "AllowLiveKitRtcUdpInbound"
      protocol    = "udp"
      port        = 7882
      priority    = 130
      source_cidr = null
      description = "LiveKit WebRTC media over multiplexed UDP"
    },
  ]

  ssh_ports = var.ssh_allowed_cidr != "" ? [
    {
      name        = "AllowSshInbound"
      protocol    = "tcp"
      port        = 22
      priority    = 140
      source_cidr = var.ssh_allowed_cidr
      description = "SSH access restricted to a single operator IP/CIDR"
    }
  ] : []

  ports = concat(local.base_ports, local.ssh_ports)
}
