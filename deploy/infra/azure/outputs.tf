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

output "schedule_workflow_names" {
  description = "Names of the provisioned start/deallocate Logic App workflows."
  value       = [for k in sort(keys(azurerm_logic_app_workflow.schedule)) : azurerm_logic_app_workflow.schedule[k].name]
}
