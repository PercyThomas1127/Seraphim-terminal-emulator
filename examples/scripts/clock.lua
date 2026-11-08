-- A one-file script (scripts/clock.lua): Ctrl+Shift+T shows the time.
-- One-file scripts get the default permissions: visual, interface, metrics.

ui.command("time", {
  title = "What Time Is It?",
  run = function()
    ui.notify(os.date("It is %H:%M"))
  end,
})
ui.key("ctrl+shift+t", "time")
