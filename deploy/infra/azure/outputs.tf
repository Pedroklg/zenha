output "public_ip_address" {
  description = "Static Standard public IPv4 address of the VM."
  value       = azurerm_public_ip.main.ip_address
}

output "fqdn" {
  description = "Azure-assigned DNS name for the public IP."
  value       = azurerm_public_ip.main.fqdn
}

output "vm_name" {
  description = "Name of the provisioned Azure Linux virtual machine."
  value       = azurerm_linux_virtual_machine.main.name
}
