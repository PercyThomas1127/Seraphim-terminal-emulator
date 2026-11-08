-- React to Gabriel package installs, as in spec §8.3. Written for the
-- `heavens` preset, whose `sky` layer has `intensity` and `install`.

animation.define("installation_start", {
  { layer = "sky", param = "install", from = 0, to = 1, duration = 1200, easing = "ease-out" },
})
animation.define("installation_success", {
  { layer = "sky", param = "install", to = 0, duration = 1500, easing = "ease-in-out" },
  { layer = "sky", color = "#9ef0c0", name = "surge", duration = 300 },
  { layer = "sky", color = "theme.bright.5", name = "surge", delay = 1500, duration = 800 },
})

seraphim.on("gabriel.install.started", function(event)
  visual.set_parameter("intensity", 0.8, { layer = "sky", duration = 600 })
  animation.play("installation_start")
  ui.notify("Installing " .. (event.package or "a package") .. "…")
end)

seraphim.on("gabriel.install.completed", function(event)
  animation.play("installation_success")
  visual.set_parameter("intensity", 0.2, { layer = "sky", duration = 2000 })
end)

seraphim.on("animation.completed", function(event)
  -- event.name is the event's own name; the animation's is in event.data.
  if event.data.name == "installation_success" and event.extension == seraphim.name then
    ui.notify("Installed.")
  end
end)
