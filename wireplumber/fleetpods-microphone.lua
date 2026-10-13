-- SPDX-License-Identifier: MIT
local linking = require ("linking-utils")
local managedAddresses = {}

SimpleEventHook {
  name = "fleetpods/microphone-target",
  before = "linking/find-defined-target",
  interests = {
    EventInterest { Constraint { "event.type", "=", "select-target" } },
  },
  execute = function (event)
    local source, objects, item, props, flags = linking:unwrap_select_target_event (event)
    -- Sample node name: bluez_capture_internal.00:11:22:33:44:55
    local address = (props["node.name"] or ""):match ("^bluez_capture_internal%.(.+)$")
    if not address then return end
    for target in objects:iterate { type = "SiLinkable" } do
      local node = target:get_associated_proxy ("node")
      if node and node.properties["node.name"] == "fleetpods.hires-microphone"
          and node.properties["api.bluez5.address"] == address then
        managedAddresses[address] = true
        flags.has_defined_target = true
        flags.has_node_defined_target = true
        flags.can_passthrough = false
        event:set_data ("target", target)
        return
      end
    end
  end,
}:register ()

SimpleEventHook {
  name = "fleetpods/keep-playback-profile",
  before = "switch-profile@autoswitch-bluetooth-profile",
  interests = {
    EventInterest { Constraint { "event.type", "=", "autoswitch-bluez-headset-profile" } },
  },
  execute = function (event)
    local source = event:get_source ()
    local devices = source:call ("get-object-manager", "device")
    local device = devices:lookup {
      Constraint { "bound-id", "=", event:get_data ("device-id"), type = "gobject" },
    }
    if not device then return end
    -- Keep the profile while a managed daemon restarts and temporarily removes its source.
    if managedAddresses[device.properties["api.bluez5.address"]] then
      event:stop_processing ()
      return
    end
    local nodes = source:call ("get-object-manager", "node")
    for node in nodes:iterate {
      Constraint { "node.name", "=", "fleetpods.hires-microphone", type = "pw" },
    } do
      if node.properties["api.bluez5.address"] == device.properties["api.bluez5.address"] then
        event:stop_processing ()
        return
      end
    end
  end,
}:register ()

local common = require ("common-utils")
local function applyInputControls (source, device)
  local nodes = source:call ("get-object-manager", "node")
  local address = device.properties["api.bluez5.address"]
  if not address then return end
  local microphone = nodes:lookup {
    Constraint { "node.name", "=", "fleetpods.hires-microphone", type = "pw" },
    Constraint { "api.bluez5.address", "=", address, type = "pw" },
  }
  if not microphone then return end
  managedAddresses[address] = true
  local input = nodes:lookup {
    Constraint { "node.name", "=", "bluez_input." .. address, type = "pw" },
  }
  if not input then return end
  for param in device:iterate_params ("Route") do
    local route = common.parseParam (param, "Route")
    if route and route.direction == "Input" and route.props then
      local props = route.props.properties
      local volumes = { "Spa:Float" }
      for _, volume in ipairs (props.channelVolumes or { 1.0 }) do
        table.insert (volumes, volume)
      end
      -- BlueZ's hardware input route does not control audio arriving over AACP.
      input:set_param ("Props", Pod.Object {
        "Spa:Pod:Object:Param:Props", "Props",
        mute = props.mute or false,
        channelVolumes = Pod.Array (volumes),
        params = Pod.Struct { "monitor.channel-volumes", true },
      })
      -- PipeWire volume on the decoded source. 1.5 is louder than unity and short of the 2.5 that was too much.
      microphone:set_param ("Props", Pod.Object {
        "Spa:Pod:Object:Param:Props", "Props",
        mute = props.mute or false,
        channelVolumes = Pod.Array { "Spa:Float", 1.5 },
      })
      return
    end
  end
end

SimpleEventHook {
  name = "fleetpods/microphone-controls",
  interests = {
    EventInterest {
      Constraint { "event.type", "=", "device-params-changed" },
      Constraint { "event.subject.param-id", "=", "Route" },
      Constraint { "device.api", "=", "bluez5" },
    },
    EventInterest { Constraint { "event.type", "=", "node-added" } },
  },
  execute = function (event)
    local source = event:get_source ()
    local devices = source:call ("get-object-manager", "device")
    for device in devices:iterate {
      Constraint { "device.api", "=", "bluez5", type = "pw" },
    } do
      applyInputControls (source, device)
    end
  end,
}:register ()
