const log = console.error.bind(console);
import("./runtime.js").catch(error => {
  document.querySelector("main").textContent = "This Mini Program page could not be opened.";
  log("Mini Program startup failed", error);
});
