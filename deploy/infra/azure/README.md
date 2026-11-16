# Zenha Azure Infrastructure (OpenTofu)

Provisions the Zenha Linux VM (`Standard_B2ats_v2`), VNet/subnet, NSG, static Standard Public IPv4 with DNS label, NIC, cloud-init Docker setup, and optional Logic App start/deallocate schedules in an existing Azure Resource Group.

## Prerequisites

1. [OpenTofu](https://opentofu.org/) (`tofu`) >= 1.6.0
2. [Azure CLI](https://learn.microsoft.com/cli/azure/install-azure-cli) (`az`) logged into the target subscription:
   ```bash
   az login
   az account set --subscription "<subscription-id>"
   export ARM_SUBSCRIPTION_ID="<subscription-id>"
   ```
   In PowerShell on Windows:
   ```powershell
   az login
   az account set --subscription "<subscription-id>"
   $env:ARM_SUBSCRIPTION_ID = "<subscription-id>"
   ```

## Fresh Deployment (`init` / `plan` / `apply`)

1. Create the resource group if it does not exist yet:
   ```bash
   az group create --name zenha-rg --location chilecentral
   ```
2. Copy `terraform.tfvars.example` to `terraform.tfvars` (git-ignored) and set `ssh_public_key` and a globally unique `dns_label_prefix`:
   ```bash
   cp terraform.tfvars.example terraform.tfvars
   ```
3. Initialize providers, inspect the plan, and apply:
   ```bash
   cd deploy/infra/azure
   tofu init
   tofu plan
   tofu apply
   ```
4. Read the outputs (`public_ip_address`, `fqdn`, `vm_name`):
   ```bash
   tofu output
   ```

## Adotar a VM existente

Use the commands below to adopt resources already created by the former Bicep templates (`main.bicep` and `schedule.bicep`, now removed from the repository) in resource group `zenha-rg` (assuming default `name_prefix = "zenha"`).

> **Warning:** Before running any `tofu apply` after importing, **always run `tofu plan` and confirm there are zero `destroy` or `replace` (`-/+`) actions** in the plan output.

### 1. Prepare configuration and initialize

1. Create `terraform.tfvars` from `terraform.tfvars.example` with the **exact same values** currently deployed (`resource_group_name = "zenha-rg"`, `location`, `name_prefix`, `vm_size`, `os_disk_size_gb`, `admin_username`, `ssh_public_key`, `dns_label_prefix`, `ssh_allowed_cidr`).
2. Set `ARM_SUBSCRIPTION_ID` (replace `<subscription-id>` with your Azure subscription GUID) and run `tofu init`.

### 2. Import core networking and VM resources

```bash
tofu import azurerm_network_security_group.main "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Network/networkSecurityGroups/zenha-nsg"
tofu import azurerm_virtual_network.main "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Network/virtualNetworks/zenha-vnet"
tofu import azurerm_subnet.default "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Network/virtualNetworks/zenha-vnet/subnets/default"
tofu import azurerm_subnet_network_security_group_association.default "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Network/virtualNetworks/zenha-vnet/subnets/default"
tofu import azurerm_public_ip.main "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Network/publicIPAddresses/zenha-pip"
tofu import azurerm_network_interface.main "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Network/networkInterfaces/zenha-nic"
tofu import azurerm_network_interface_security_group_association.main "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Network/networkInterfaces/zenha-nic"
tofu import azurerm_linux_virtual_machine.main "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Compute/virtualMachines/zenha-vm"
```

### 3. Import schedule workflows, triggers, actions, and role assignments

If the schedule was not deployed yet and you do not want schedules, set `schedule_enabled = false` in `terraform.tfvars` and skip this step. Otherwise, keep `schedule_enabled = true` and import the four Logic App workflows and their child resources:

```bash
tofu import 'azurerm_logic_app_workflow.schedule["start-mon-fri"]' "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Logic/workflows/zenha-schedule-start-mon-fri"
tofu import 'azurerm_logic_app_workflow.schedule["start-sat-sun"]' "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Logic/workflows/zenha-schedule-start-sat-sun"
tofu import 'azurerm_logic_app_workflow.schedule["deallocate-tue-sat"]' "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Logic/workflows/zenha-schedule-deallocate-tue-sat"
tofu import 'azurerm_logic_app_workflow.schedule["deallocate-sun-mon"]' "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Logic/workflows/zenha-schedule-deallocate-sun-mon"

tofu import 'azurerm_logic_app_trigger_recurrence.schedule["start-mon-fri"]' "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Logic/workflows/zenha-schedule-start-mon-fri/triggers/Recurrence"
tofu import 'azurerm_logic_app_trigger_recurrence.schedule["start-sat-sun"]' "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Logic/workflows/zenha-schedule-start-sat-sun/triggers/Recurrence"
tofu import 'azurerm_logic_app_trigger_recurrence.schedule["deallocate-tue-sat"]' "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Logic/workflows/zenha-schedule-deallocate-tue-sat/triggers/Recurrence"
tofu import 'azurerm_logic_app_trigger_recurrence.schedule["deallocate-sun-mon"]' "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Logic/workflows/zenha-schedule-deallocate-sun-mon/triggers/Recurrence"

tofu import 'azurerm_logic_app_action_custom.schedule["start-mon-fri"]' "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Logic/workflows/zenha-schedule-start-mon-fri/actions/VmPowerAction"
tofu import 'azurerm_logic_app_action_custom.schedule["start-sat-sun"]' "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Logic/workflows/zenha-schedule-start-sat-sun/actions/VmPowerAction"
tofu import 'azurerm_logic_app_action_custom.schedule["deallocate-tue-sat"]' "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Logic/workflows/zenha-schedule-deallocate-tue-sat/actions/VmPowerAction"
tofu import 'azurerm_logic_app_action_custom.schedule["deallocate-sun-mon"]' "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Logic/workflows/zenha-schedule-deallocate-sun-mon/actions/VmPowerAction"
```

Next, look up the GUID of each `Virtual Machine Contributor` role assignment created by the former schedule template on `zenha-vm`:

```bash
az role assignment list \
  --scope "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Compute/virtualMachines/zenha-vm" \
  --query "[?roleDefinitionName=='Virtual Machine Contributor'].{id:id, principalId:principalId}" \
  -o table

az resource list \
  -g zenha-rg \
  --resource-type Microsoft.Logic/workflows \
  --query "[?starts_with(name, 'zenha-schedule-')].{name:name, principalId:identity.principalId}" \
  -o table
```

Match each workflow's `principalId` to its role assignment `id`, then import the four role assignments (replace `<role-assignment-guid-*>` with the matching GUIDs):

```bash
tofu import 'azurerm_role_assignment.schedule_vm_contributor["start-mon-fri"]' "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Compute/virtualMachines/zenha-vm/providers/Microsoft.Authorization/roleAssignments/<role-assignment-guid-start-mon-fri>"
tofu import 'azurerm_role_assignment.schedule_vm_contributor["start-sat-sun"]' "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Compute/virtualMachines/zenha-vm/providers/Microsoft.Authorization/roleAssignments/<role-assignment-guid-start-sat-sun>"
tofu import 'azurerm_role_assignment.schedule_vm_contributor["deallocate-tue-sat"]' "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Compute/virtualMachines/zenha-vm/providers/Microsoft.Authorization/roleAssignments/<role-assignment-guid-deallocate-tue-sat>"
tofu import 'azurerm_role_assignment.schedule_vm_contributor["deallocate-sun-mon"]' "/subscriptions/<subscription-id>/resourceGroups/zenha-rg/providers/Microsoft.Compute/virtualMachines/zenha-vm/providers/Microsoft.Authorization/roleAssignments/<role-assignment-guid-deallocate-sun-mon>"
```

*(Windows PowerShell tip: if PowerShell strips inner quotes inside `'...[ "key" ]'`, escape the inner double quotes like `'azurerm_logic_app_workflow.schedule[\"start-mon-fri\"]'`.)*

### 4. Verify with `tofu plan`

```bash
tofu plan
```

Confirm that the plan shows **0 to destroy** and **0 to replace** before running `tofu apply`.
