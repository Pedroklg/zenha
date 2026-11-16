output "public_ip_address" {
  description = "Static Primary IPv4 address assigned to the Hetzner Cloud server."
  value       = hcloud_primary_ip.ipv4.ip_address
}

output "public_ipv6_address" {
  description = "Public IPv6 address assigned to the Hetzner Cloud server (empty if disabled)."
  value       = hcloud_server.main.ipv6_address
}

output "address" {
  description = "Public address to target in DNS records (Hetzner Cloud does not provision a cloudapp FQDN)."
  value       = hcloud_primary_ip.ipv4.ip_address
}

output "vm_name" {
  description = "Name of the provisioned Hetzner Cloud server."
  value       = hcloud_server.main.name
}
