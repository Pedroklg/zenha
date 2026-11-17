# Azure infrastructure (OpenTofu)

Creates the Zenha VM in an existing resource group: VNet and subnet, NSG (ports from [`../modules/host`](../modules/host/main.tf)), static Standard public IPv4 with a DNS label, NIC, a `Standard_B2ats_v2` Ubuntu 24.04 VM with cloud-init.

| File | Contents |
| --- | --- |
| [`main.tf`](main.tf) | Network, IP, NIC, VM |
| [`variables.tf`](variables.tf) | Inputs and defaults (`zenha-rg`, `chilecentral`, `zenha` prefix) |
| [`outputs.tf`](outputs.tf) | `public_ip_address`, `fqdn`, `vm_name` |
| [`terraform.tfvars.example`](terraform.tfvars.example) | Copy to `terraform.tfvars` (gitignored) |

Requires OpenTofu 1.6+ and the Azure CLI logged in with `ARM_SUBSCRIPTION_ID` set.

## New deployment

Follow [Provision the VM](../../../docs/deploy.md#2-provision-the-vm) in the deployment guide. In short:

```bash
az group create --name zenha-rg --location chilecentral
cd deploy/infra/azure
cp terraform.tfvars.example terraform.tfvars   # set ssh_public_key and dns_label_prefix
tofu init
tofu plan
tofu apply
tofu output
```

## Adopting an existing deployment

These steps adopt resources created by the former Bicep templates (`main.bicep` and `schedule.bicep`, now removed) in `zenha-rg`, with the default `name_prefix = "zenha"`.

> Before any `tofu apply` after importing, run `tofu plan` and confirm it shows **0 to destroy** and **0 to replace** (`-/+`).

1. Write `terraform.tfvars` with the values that are deployed now: `resource_group_name`, `location`, `name_prefix`, `vm_size`, `os_disk_size_gb`, `admin_username`, `ssh_public_key`, `dns_label_prefix`, `ssh_allowed_cidr`. Then run `tofu init`.
2. Import the network and the VM (bash; in Git Bash set `MSYS_NO_PATHCONV=1` first):
   ```bash
   RG="/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers"
   tofu import azurerm_network_security_group.main "$RG/Microsoft.Network/networkSecurityGroups/zenha-nsg"
   tofu import azurerm_virtual_network.main "$RG/Microsoft.Network/virtualNetworks/zenha-vnet"
   tofu import azurerm_subnet.default "$RG/Microsoft.Network/virtualNetworks/zenha-vnet/subnets/default"
   tofu import azurerm_subnet_network_security_group_association.default "$RG/Microsoft.Network/virtualNetworks/zenha-vnet/subnets/default"
   tofu import azurerm_public_ip.main "$RG/Microsoft.Network/publicIPAddresses/zenha-pip"
   tofu import azurerm_network_interface.main "$RG/Microsoft.Network/networkInterfaces/zenha-nic"
   tofu import azurerm_network_interface_security_group_association.main "$RG/Microsoft.Network/networkInterfaces/zenha-nic"
   tofu import azurerm_linux_virtual_machine.main "$RG/Microsoft.Compute/virtualMachines/zenha-vm"
   ```
3. Run `tofu plan` and check for 0 to destroy and 0 to replace before `tofu apply`.
