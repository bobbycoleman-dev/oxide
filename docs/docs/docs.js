// Oxide docs — progressive enhancement only; every page reads fine without it.
(function () {
  var article = document.querySelector("article");

  // Mobile sidebar toggle.
  var btn = document.querySelector(".menu-btn");
  var sidebar = document.getElementById("sidebar");
  if (btn && sidebar) {
    btn.addEventListener("click", function () {
      var open = sidebar.classList.toggle("open");
      btn.setAttribute("aria-expanded", open ? "true" : "false");
    });
  }

  // Copy buttons on code blocks.
  document.querySelectorAll("pre").forEach(function (pre) {
    var b = document.createElement("button");
    b.className = "copy-btn";
    b.type = "button";
    b.textContent = "copy";
    b.addEventListener("click", function () {
      navigator.clipboard.writeText(pre.innerText.replace(/\ncopy$/, "")).then(
        function () {
          b.textContent = "copied";
          setTimeout(function () {
            b.textContent = "copy";
          }, 1400);
        },
        function () {
          b.textContent = "failed";
        }
      );
    });
    pre.appendChild(b);
  });

  if (!article) return;

  // Anchor links on headings, and the "on this page" rail.
  var toc = document.getElementById("toc");
  var headings = article.querySelectorAll("h2[id], h3[id]");
  var links = [];

  headings.forEach(function (h) {
    var a = document.createElement("a");
    a.className = "anchor";
    a.href = "#" + h.id;
    a.setAttribute("aria-label", "Link to this section");
    a.textContent = "#";
    h.appendChild(a);

    if (!toc) return;
    var link = document.createElement("a");
    link.href = "#" + h.id;
    link.textContent = h.textContent.replace(/#$/, "").trim();
    if (h.tagName === "H3") link.className = "sub";
    toc.appendChild(link);
    links.push(link);
  });

  if (toc && links.length) {
    var title = document.createElement("div");
    title.className = "toc-title";
    title.textContent = "On this page";
    toc.insertBefore(title, toc.firstChild);
  } else if (toc) {
    toc.style.display = "none";
  }

  // Scrollspy: highlight the last heading scrolled past.
  if (links.length) {
    var mark = function () {
      var top = window.scrollY + 90;
      var current = 0;
      headings.forEach(function (h, i) {
        if (h.offsetTop <= top) current = i;
      });
      links.forEach(function (l, i) {
        l.classList.toggle("active", i === current);
      });
    };
    mark();
    var ticking = false;
    window.addEventListener(
      "scroll",
      function () {
        if (ticking) return;
        ticking = true;
        requestAnimationFrame(function () {
          mark();
          ticking = false;
        });
      },
      { passive: true }
    );
  }

  var year = document.getElementById("year");
  if (year) year.textContent = new Date().getFullYear();
})();
