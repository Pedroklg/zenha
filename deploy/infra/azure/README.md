# Azure infrastructure (OpenTofu)

Creates the Zenha VM in an existing resource group: VNet and subnet, NSG (ports from [`../modules/host`](../modules/host/main.tf)), static Standard public IPv4 with a DNS label, NIC, a `Standard_B2ats_v2` Ubuntu 24.04 VM with cloud-init, and optionally the start/stop schedule ([`schedule.tf`](schedule.tf)).

| File | Contents |
| --- | --- |
| [`main.tf`](main.tf) | Network, IP, NIC, VM |
| [`schedule.tf`](schedule.tf) | Logic App workflows and their role assignments |
| [`variables.tf`](variables.tf) | Inputs and defaults (`zenha-rg`, `chilecentral`, `zenha` prefix, schedule times) |
| [`outputs.tf`](outputs.tf) | `public_ip_address`, `fqdn`, `vm_name`, `schedule_workflow_names` |
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
3. Import the schedule. If it was never deployed and you do not want it, set `schedule_enabled = false` and skip to step 4.
   ```bash
   for s in start-mon-fri start-sat-sun deallocate-tue-sat deallocate-sun-mon; do
     wf="$RG/Microsoft.Logic/workflows/zenha-schedule-$s"
     tofu import "azurerm_logic_app_workflow.schedule[\"$s\"]" "$wf"
     tofu import "azurerm_logic_app_trigger_recurrence.schedule[\"$s\"]" "$wf/triggers/Recurrence"
     tofu import "azurerm_logic_app_action_custom.schedule[\"$s\"]" "$wf/actions/VmPowerAction"
   done
   ```
   Find the `Virtual Machine Contributor` role assignment of each workflow by matching its `principalId`:
   ```bash
   az role assignment list \
     --scope "$RG/Microsoft.Compute/virtualMachines/zenha-vm" \
     --query "[?roleDefinitionName=='Virtual Machine Contributor'].{id:id, principalId:principalId}" \
     -o table
   az resource list -g zenha-rg --resource-type Microsoft.Logic/workflows \
     --query "[?starts_with(name, 'zenha-schedule-')].{name:name, principalId:identity.principalId}" \
     -o table
   ```
   Then import each one with its GUID:
   ```bash
   RA="$RG/Microsoft.Compute/virtualMachines/zenha-vm/providers/Microsoft.Authorization/roleAssignments"
   tofu import 'azurerm_role_assignment.schedule_vm_contributor["start-mon-fri"]' "$RA/<guid-start-mon-fri>"
   tofu import 'azurerm_role_assignment.schedule_vm_contributor["start-sat-sun"]' "$RA/<guid-start-sat-sun>"
   tofu import 'azurerm_role_assignment.schedule_vm_contributor["deallocate-tue-sat"]' "$RA/<guid-deallocate-tue-sat>"
   tofu import 'azurerm_role_assignment.schedule_vm_contributor["deallocate-sun-mon"]' "$RA/<guid-deallocate-sun-mon>"
   ```
4. Run `tofu plan` and check for 0 to destroy and 0 to replace before `tofu apply`.
