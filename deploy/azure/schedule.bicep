@description('Azure region for the schedule Logic Apps.')
param location string = resourceGroup().location

@description('Name of the existing Virtual Machine to start and deallocate.')
param vmName string = 'zenha-vm'

@description('Name prefix for the schedule Logic App workflows.')
param workflowPrefix string = 'zenha-schedule'

@description('Whether the schedule workflows are enabled.')
param enabled bool = true

@description('Windows time zone name for the recurrence trigger (America/Sao_Paulo).')
param timeZone string = 'E. South America Standard Time'

@description('Schedule rules: name suffix, days, hour (0-23), optional minute (0-59), and VM action (start or deallocate).')
param schedules array = [
  {
    name: 'start-mon-fri'
    days: [
      'Monday'
      'Tuesday'
      'Wednesday'
      'Thursday'
      'Friday'
    ]
    hour: 18
    action: 'start'
  }
  {
    name: 'start-sat-sun'
    days: [
      'Saturday'
      'Sunday'
    ]
    hour: 11
    action: 'start'
  }
  {
    name: 'deallocate-tue-sat'
    days: [
      'Tuesday'
      'Wednesday'
      'Thursday'
      'Friday'
      'Saturday'
    ]
    hour: 1
    action: 'deallocate'
  }
  {
    name: 'deallocate-sun-mon'
    days: [
      'Sunday'
      'Monday'
    ]
    hour: 2
    action: 'deallocate'
  }
]

var vmContributorRoleDefinitionId = subscriptionResourceId(
  'Microsoft.Authorization/roleDefinitions',
  '9980e02c-c2be-4d73-94e8-173b1dc7cf3c'
)

resource vm 'Microsoft.Compute/virtualMachines@2024-07-01' existing = {
  name: vmName
}

resource workflows 'Microsoft.Logic/workflows@2019-05-01' = [
  for item in schedules: {
    name: '${workflowPrefix}-${item.name}'
    location: location
    identity: {
      type: 'SystemAssigned'
    }
    properties: {
      state: enabled ? 'Enabled' : 'Disabled'
      definition: {
        '$schema': 'https://schema.management.azure.com/providers/Microsoft.Logic/schemas/2016-06-01/workflowdefinition.json#'
        contentVersion: '1.0.0.0'
        parameters: {}
        triggers: {
          Recurrence: {
            type: 'Recurrence'
            recurrence: {
              frequency: 'Week'
              interval: 1
              timeZone: timeZone
              schedule: {
                weekDays: item.days
                hours: [
                  item.hour
                ]
                minutes: [
                  item.?minute ?? 0
                ]
              }
            }
          }
        }
        actions: {
          VmPowerAction: {
            type: 'Http'
            runAfter: {}
            operationOptions: 'DisableAsyncPattern'
            inputs: {
              method: 'POST'
              uri: '${environment().resourceManager}${skip(vm.id, 1)}/${item.action}?api-version=2024-07-01'
              authentication: {
                type: 'ManagedServiceIdentity'
                audience: environment().resourceManager
              }
            }
          }
        }
        outputs: {}
      }
      parameters: {}
    }
  }
]

resource roleAssignments 'Microsoft.Authorization/roleAssignments@2022-04-01' = [
  for (item, i) in schedules: {
    name: guid(vm.id, '${workflowPrefix}-${item.name}', vmContributorRoleDefinitionId)
    scope: vm
    properties: {
      roleDefinitionId: vmContributorRoleDefinitionId
      principalId: workflows[i].identity.principalId
      principalType: 'ServicePrincipal'
    }
  }
]

output workflowNames array = [for (item, i) in schedules: workflows[i].name]
