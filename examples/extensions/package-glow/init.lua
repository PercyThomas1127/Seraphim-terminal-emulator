-- React to package installs and upgrades by any package manager Seraphim
-- recognizes (emerge, pacman, paru, yay, apt, dnf, zypper, xbps, apk,
-- eopkg, nix, flatpak, snap, brew). Written for the `heavens` preset,
-- whose `sky` layer has `intensity` and `install`.

animation.define("installation_start", {
  { layer = "sky", param = "install", from = 0, to = 1, duration = 1200, easing = "ease-out" },
})
animation.define("installation_success", {
  { layer = "sky", param = "install", to = 0, duration = 1500, easing = "ease-in-out" },
  { layer = "sky", color = "#9ef0c0", name = "surge", duration = 300 },
  { layer = "sky", color = "theme.bright.5", name = "surge", delay = 1500, duration = 800 },
})

local function started(event)
  visual.set_parameter("intensity", 0.8, { layer = "sky", duration = 600 })
  animation.play("installation_start")
  local what = event.package or "the system"
  ui.notify(event.manager .. ": " .. (event.operation == "upgrade" and "upgrading " or "installing ") .. what .. "…")
end

-- Progress (when the manager prints it): the sky brightens as it goes.
local function progress(event)
  if event.percent then
    visual.set_parameter("intensity", 0.3 + 0.6 * event.percent / 100, { layer = "sky", duration = 300 })
  end
end

local function completed(event)
  animation.play("installation_success")
  visual.set_parameter("intensity", 0.2, { layer = "sky", duration = 2000 })
end

local function failed(event)
  visual.set_parameter("install", 0, { layer = "sky", duration = 400 })
  visual.set_parameter("intensity", 0.2, { layer = "sky", duration = 800 })
  ui.notify(event.manager .. " failed (exit " .. tostring(event.exit_code) .. ")")
end

for _, op in ipairs({ "install", "upgrade" }) do
  seraphim.on("package." .. op .. ".started", started)
  seraphim.on("package." .. op .. ".progress", progress, { throttle = 100 })
  seraphim.on("package." .. op .. ".completed", completed)
  seraphim.on("package." .. op .. ".failed", failed)
end

seraphim.on("animation.completed", function(event)
  -- event.name is the event's own name; the animation's is in event.data.
  if event.data.name == "installation_success" and event.extension == seraphim.name then
    ui.notify("Done.")
  end
end)
