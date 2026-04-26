// Client-side fuzzy search with multi-signal scoring + match highlighting.
(function () {
  "use strict";

  var searchInput = document.getElementById("search-input");
  var searchResults = document.getElementById("search-results");
  if (!searchInput || !searchResults) return;

  var searchIndex = null;
  fetch("/search-index.json")
    .then(function (r) { return r.json(); })
    .then(function (data) { searchIndex = data; })
    .catch(function () {});

  function escapeRegex(s) { return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"); }
  function escapeHtml(str) {
    var div = document.createElement("div");
    div.textContent = str;
    return div.innerHTML;
  }
  function isWordBoundary(t, i) {
    if (i === 0) return true;
    var c = t[i - 1];
    return c === " " || c === "/" || c === "-" || c === "_" || c === ".";
  }

  // Acronym match: query "kg" hits "knowledge graph", "ai" hits
  // "artificial intelligence", "ddd" hits "domain driven design".
  // Returns score > 0 when every query char is the first letter of a
  // word in target, in order. Short acronyms are common in the index
  // (`ai`, `cs`) so we require length >= 2.
  function acronymMatch(query, target) {
    var q = query.replace(/\s+/g, "").toLowerCase();
    if (q.length < 2) return 0;
    var t = target.toLowerCase();
    var firsts = "";
    for (var i = 0; i < t.length; i++) {
      if (isWordBoundary(t, i) && /[a-z0-9]/.test(t[i])) firsts += t[i];
    }
    var idx = firsts.indexOf(q);
    if (idx === -1) return 0;
    // Reward a tight initial-letter match — strongest when q covers
    // every word (e.g. "kg" on a 2-word title) and shrinks as the
    // target grows wordier.
    var coverage = q.length / Math.max(firsts.length, q.length);
    return 60 + q.length * 8 + coverage * 40;
  }

  function fuzzyMatch(query, target) {
    var q = query.toLowerCase();
    var t = target.toLowerCase();

    var idx = t.indexOf(q);
    if (idx !== -1) {
      var base = 100 + q.length * 10;
      if (idx === 0) base += 50;
      else if (isWordBoundary(t, idx)) base += 30;
      // Coverage bonus: rewards a query that consumes most/all of
      // the target. Without this "cyb" matched "cyb" and
      // "cyber/whitepaper" with the same score (both got start-of-
      // string + length), so the longer-named pages outscored the
      // exact-title page once excerpt/tag noise was added in.
      var coverage = q.length / t.length;
      base += coverage * 60;
      return base;
    }

    // All query words appear as word-prefix tokens.
    var qWords = q.split(/\s+/).filter(Boolean);
    if (qWords.length > 1) {
      var allFound = true;
      for (var w = 0; w < qWords.length; w++) {
        var re = new RegExp("(^|[\\s/\\-_.])" + escapeRegex(qWords[w]));
        if (!re.test(t)) { allFound = false; break; }
      }
      if (allFound) return 80 + q.length * 5;
    }

    // Sequential char match with gap penalty.
    var qi = 0, ti = 0, score = 0, consecutive = 0, firstMatch = -1;
    while (qi < q.length && ti < t.length) {
      if (q[qi] === t[ti]) {
        if (firstMatch === -1) firstMatch = ti;
        consecutive++;
        score += consecutive * 2;
        if (isWordBoundary(t, ti)) score += 5;
        qi++;
      } else {
        consecutive = 0;
      }
      ti++;
    }
    if (qi < q.length) return 0;
    var spread = ti - firstMatch;
    var density = q.length / spread;
    score = score * density;
    return Math.max(score, 1);
  }

  function bestScore(query, target) {
    if (!target) return 0;
    return Math.max(fuzzyMatch(query, target), acronymMatch(query, target));
  }

  // Score = (title*3 + bestTag*2 + excerpt*0.6) × graphPrior.
  //
  // graphPrior folds the tri-kernel focus and gravity percentiles
  // (pre-computed in Rust, shipped 0..1) into a multiplier on the
  // text-match score. The old `sqrt(focus)*50` additive boost was
  // dwarfed by title scores in the 200–400 range — focus values
  // are tiny fractions (Σ=1 over ~22k pages) so it barely moved
  // ranking. As a multiplier on already-percentile-normalized
  // values it has predictable, meaningful effect:
  //   - bottom-rank page: prior = 0.7
  //   - median page:      prior = 1.15
  //   - top hub:          prior = 1.7
  // A perfect title match still wins outright, but among similarly-
  // matched pages the more central ones float to the top.
  function scoreEntry(entry, query) {
    var titleScore = bestScore(query, entry.title) * 3;
    var tagScore = 0;
    if (entry.tags) {
      for (var i = 0; i < entry.tags.length; i++) {
        var s = bestScore(query, entry.tags[i]) * 2;
        if (s > tagScore) tagScore = s;
      }
    }
    var excerptScore = bestScore(query, entry.excerpt || "") * 0.6;
    var matchScore = titleScore + tagScore + excerptScore;
    if (matchScore <= 0) return 0;
    var f = entry.focus_pct || 0;
    var g = entry.gravity_pct || 0;
    var graphPrior = 0.7 + 0.6 * f + 0.4 * g;
    return matchScore * graphPrior;
  }

  // Highlight every contiguous case-insensitive substring of `query`
  // in `text`. For multi-word queries each word is highlighted
  // independently so partial matches still light up.
  function highlight(text, query) {
    if (!text || !query) return escapeHtml(text || "");
    var parts = query.trim().split(/\s+/).filter(Boolean);
    parts.sort(function (a, b) { return b.length - a.length; });
    var pattern = parts.map(escapeRegex).join("|");
    if (!pattern) return escapeHtml(text);
    var re = new RegExp("(" + pattern + ")", "gi");
    return escapeHtml(text).replace(
      new RegExp(
        "(" +
          parts.map(function (p) {
            return escapeHtml(p).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
          }).join("|") +
          ")",
        "gi"
      ),
      '<mark>$1</mark>'
    );
  }

  var activeIndex = -1;
  function updateActive() {
    var items = searchResults.querySelectorAll("a");
    for (var i = 0; i < items.length; i++) {
      if (i === activeIndex) {
        items[i].classList.add("active");
        items[i].scrollIntoView({ block: "nearest" });
      } else {
        items[i].classList.remove("active");
      }
    }
  }

  searchInput.addEventListener("input", function () {
    var query = this.value.trim();
    activeIndex = -1;

    if (!query || !searchIndex) {
      searchResults.innerHTML = "";
      return;
    }

    var scored = [];
    for (var i = 0; i < searchIndex.length; i++) {
      var s = scoreEntry(searchIndex[i], query);
      if (s > 0) scored.push({ entry: searchIndex[i], score: s });
    }
    scored.sort(function (a, b) { return b.score - a.score; });
    var results = scored.slice(0, 12);

    if (!results.length) {
      searchResults.innerHTML = '<p class="search-empty">no results</p>';
      return;
    }

    searchResults.innerHTML = results.map(function (r) {
      var titleHtml = highlight(r.entry.title, query);
      var excerpt = r.entry.excerpt ? r.entry.excerpt.substring(0, 120) : "";
      var excerptHtml = excerpt ? highlight(excerpt, query) : "";
      var tagsHtml = "";
      if (r.entry.tags && r.entry.tags.length) {
        tagsHtml = '<span class="search-result-tags">' +
          r.entry.tags.slice(0, 4).map(function (t) {
            return '<span class="search-result-tag">' + escapeHtml(t) + '</span>';
          }).join("") +
          '</span>';
      }
      return '<a href="' + r.entry.url + '">' +
        '<div class="search-result-title">' + titleHtml + tagsHtml + '</div>' +
        (excerptHtml ? '<div class="search-result-excerpt">' + excerptHtml + '</div>' : '') +
        '</a>';
    }).join("");
  });

  searchInput.addEventListener("keydown", function (e) {
    var items = searchResults.querySelectorAll("a");
    if (!items.length) return;

    // Dropdown renders above the bar, so the visual "first" item is
    // at the bottom of the list (closest to the input). ArrowUp walks
    // upward through the rendered list (toward better matches), which
    // is what the eye expects.
    if (e.key === "ArrowUp") {
      e.preventDefault();
      activeIndex = activeIndex <= 0 ? items.length - 1 : activeIndex - 1;
      if (activeIndex === -1) activeIndex = 0;
      updateActive();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      activeIndex = activeIndex < 0 ? 0 : (activeIndex + 1) % items.length;
      updateActive();
    } else if (e.key === "Enter" && activeIndex >= 0 && items[activeIndex]) {
      e.preventDefault();
      items[activeIndex].click();
    }
  });

  document.addEventListener("keydown", function (e) {
    if (e.key === "/" && document.activeElement !== searchInput) {
      e.preventDefault();
      searchInput.focus();
    }
    if (e.key === "Escape" && document.activeElement === searchInput) {
      searchInput.blur();
      searchResults.innerHTML = "";
      activeIndex = -1;
    }
  });
})();
