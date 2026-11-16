output "ports" {
  description = "Inbound firewall/NSG rules shared across cloud providers."
  value       = local.ports
}

output "cloud_init" {
  description = "Rendered cloud-init configuration for the Zenha host."
  value       = file("${path.module}/cloud-init.yaml")
}
