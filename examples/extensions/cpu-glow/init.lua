-- The sky brightens with CPU load (spec §11.4), and a command reports it.

visual.bind("sky.intensity", {
  source = "system.cpu",
  min = 0.1,
  max = 1.0,
  smoothing = 800,
})

ui.command("cpu", {
  title = "Show CPU and Memory",
  run = function()
    local cpu, mem = system.cpu(), system.memory()
    ui.notify(string.format("CPU %.0f%%, memory %.0f%%", cpu or 0, mem or 0))
  end,
})
