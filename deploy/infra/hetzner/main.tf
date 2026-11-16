module "host" {
  source = "../modules/host"

  ssh_allowed_cidr = var.ssh_allowed_cidr
}

resource "hcloud_ssh_key" "main" {
  name       = "${var.name_prefix}-ssh-key"
  public_key = trimspace(var.ssh_public_key)
}

resource "hcloud_primary_ip" "ipv4" {
  name        = "${var.name_prefix}-pip"
  type        = "ipv4"
  location    = var.location
  auto_delete = false
}

resource "hcloud_firewall" "main" {
  name = "${var.name_prefix}-fw"

  dynamic "rule" {
    for_each = module.host.ports
    content {
      direction   = "in"
      protocol    = rule.value.protocol
      port        = tostring(rule.value.port)
      source_ips  = rule.value.source_cidr != null ? [rule.value.source_cidr] : ["0.0.0.0/0", "::/0"]
      description = rule.value.description
    }
  }
}

resource "hcloud_server" "main" {
  name         = "${var.name_prefix}-vm"
  server_type  = var.server_type
  image        = var.image
  location     = var.location
  ssh_keys     = [hcloud_ssh_key.main.id]
  firewall_ids = [hcloud_firewall.main.id]
  user_data    = module.host.cloud_init

  public_net {
    ipv4_enabled = true
    ipv4         = hcloud_primary_ip.ipv4.id
    ipv6_enabled = var.enable_ipv6
  }

  # Hetzner recreates servers if ssh_keys changes after initial creation.
  lifecycle {
    ignore_changes = [ssh_keys]
  }
}
