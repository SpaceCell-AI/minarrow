// Makes the right-hand table of contents collapsible at the top category level.
// Each top-level entry that has nested entries starts collapsed and toggles open
// on click. Re-runs on every instant-navigation page load.
function setupTocCollapse() {
  const items = document.querySelectorAll(
    ".md-sidebar--secondary .md-nav--secondary > .md-nav__list > .md-nav__item"
  );
  items.forEach((item) => {
    const child = item.querySelector(":scope > nav.md-nav");
    const link = item.querySelector(":scope > a.md-nav__link");
    if (!child || !link || item.dataset.tocCollapseReady) {
      return;
    }
    item.dataset.tocCollapseReady = "1";
    item.classList.add("toc-collapsible", "toc-collapsed");
    link.addEventListener("click", (event) => {
      event.preventDefault();
      item.classList.toggle("toc-collapsed");
    });
  });
}

if (typeof document$ !== "undefined") {
  document$.subscribe(setupTocCollapse);
} else {
  document.addEventListener("DOMContentLoaded", setupTocCollapse);
}
