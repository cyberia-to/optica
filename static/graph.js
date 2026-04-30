// Graph visualization — Canvas-based, instant render for large graphs
(function() {
  const container = document.getElementById('graph-container');
  if (!container) return;

  if (window.d3) {
    initGraph();
  } else {
    const script = document.createElement('script');
    script.src = 'https://d3js.org/d3.v7.min.js';
    script.onload = () => initGraph();
    script.onerror = () => {
      container.innerHTML = '<p style="color:var(--color-text);opacity:0.5;text-align:center;padding-top:40vh">Could not load D3.js — check network connection</p>';
    };
    document.head.appendChild(script);
  }

  function initGraph() {
    if (window.__GRAPH_DATA) {
      try { renderGraph(window.__GRAPH_DATA); }
      catch (err) {
        container.innerHTML = '<p style="color:var(--color-text);opacity:0.5;text-align:center;padding-top:40vh">Graph error: ' + err.message + '</p>';
        console.error(err);
      }
    } else {
      fetch('/graph-data.json')
        .then(r => { if (!r.ok) throw new Error(r.status + ' ' + r.statusText); return r.json(); })
        .then(data => renderGraph(data))
        .catch(err => {
          container.innerHTML = '<p style="color:var(--color-text);opacity:0.5;text-align:center;padding-top:40vh">Failed to load graph data: ' + err.message + '</p>';
          console.error(err);
        });
    }
  }

  function renderGraph(data) {
    const dpr = window.devicePixelRatio || 1;
    const w = container.clientWidth || window.innerWidth;
    const h = container.clientHeight || window.innerHeight;

    // Canvas setup
    const canvas = document.createElement('canvas');
    canvas.width = w * dpr;
    canvas.height = h * dpr;
    canvas.style.width = w + 'px';
    canvas.style.height = h + 'px';
    container.appendChild(canvas);
    const ctx = canvas.getContext('2d');
    ctx.scale(dpr, dpr);

    // Read CSS colors
    const style = getComputedStyle(document.documentElement);
    const colorPrimary = style.getPropertyValue('--color-primary').trim() || '#22c55e';
    const colorText = style.getPropertyValue('--color-text').trim() || '#e5e5e5';
    const colorBg = style.getPropertyValue('--color-bg').trim() || '#0a0a0a';

    // Pre-compute adjacency
    const adj = {};
    data.nodes.forEach(n => { adj[n.id] = new Set(); });
    data.edges.forEach(e => {
      adj[e.source] = adj[e.source] || new Set();
      adj[e.target] = adj[e.target] || new Set();
      adj[e.source].add(e.target);
      adj[e.target].add(e.source);
    });

    const maxFocus = d3.max(data.nodes, d => d.focus) || 0.001;
    const radiusScale = d3.scalePow().exponent(0.4).domain([0, maxFocus]).range([1.5, 20]);
    const opacityScale = d3.scalePow().exponent(0.4).domain([0, maxFocus]).range([0.08, 0.85]);

    // Top labeled nodes — by focus (identifies true hubs)
    const sorted = [...data.nodes].sort((a, b) => (b.focus || 0) - (a.focus || 0));
    const labelCount = Math.min(12, Math.max(3, Math.floor(data.nodes.length * 0.002)));
    const labelSet = new Set(sorted.slice(0, labelCount).map(n => n.id));

    // Node index for fast lookup
    const nodeById = {};
    data.nodes.forEach(n => { nodeById[n.id] = n; });

    // Transform state (must be before finishSetup is called)
    let transform = d3.zoomIdentity;
    let hoveredNode = null;
    let hoveredCentroid = null; // active super-node under cursor in tab preview
    let centroidMembers = null; // Set<id> of pages belonging to hoveredCentroid
    let edgeRefs = [];
    let zoomBehavior = null;  // assigned in finishSetup, used by fitToVisible
    // Cluster centroids per dimension. Computed after layout
    // normalization; used to render labeled super-nodes when a tab is
    // previewed (e.g. clicking "Subgraphs" shows one circle per repo
    // at the mean position of its pages).
    const groups = { domains: {}, subgraphs: {}, tags: {} };

    // Stats element (created early so filter can update it)
    const stats = document.createElement('div');
    stats.className = 'graph-stats';

    // Three filter dimensions, combined with AND across, OR within.
    // - Domains  : crystal-domain frontmatter values (semantic)
    // - Subgraphs: which repo a page came from (structural)
    // - Tags     : long-tail folksonomy (top-N by frequency)
    // Only one dimension's pills are visible at a time; selections
    // in the others persist and keep affecting the filter.
    const activeFilters = {
      domains: new Set(),
      subgraphs: new Set(),
      tags: new Set(),
    };
    let activeDim = 'domains';
    let tabHighlight = false; // true after a tab click with no pills selected:
                              // dims nodes that have NO value in active dim
    let visibleNodes = null; // null = show all, Set = filtered

    const allDomains = (data.domains || []).slice();
    const allSubgraphs = (data.subgraphs || []).slice();
    const tagCounts = {};
    data.nodes.forEach(n => {
      (n.tags || []).forEach(t => { tagCounts[t] = (tagCounts[t] || 0) + 1; });
    });
    const topTags = Object.entries(tagCounts)
      .filter(([, count]) => count >= 3)
      .sort((a, b) => b[1] - a[1])
      .slice(0, 30)
      .map(([tag]) => tag);

    const dimensionMeta = {
      domains:   { label: 'Domains',   values: allDomains,   validator: (v) => allDomains.includes(v) },
      subgraphs: { label: 'Subgraphs', values: allSubgraphs, validator: (v) => allSubgraphs.includes(v) },
      tags:      { label: 'Tags',      values: topTags,      validator: (v) => tagCounts[v] != null },
    };

    // URL params: dim=domains|subgraphs|tags, domains=a,b, sub=c, tags=d
    const urlParams = new URLSearchParams(window.location.search);
    const urlDim = urlParams.get('dim');
    if (urlDim && dimensionMeta[urlDim]) activeDim = urlDim;
    ['domains','subgraphs','tags'].forEach(d => {
      const key = d === 'subgraphs' ? 'sub' : d;
      const raw = urlParams.get(key);
      if (!raw) return;
      raw.split(',').forEach(v => {
        const t = v.trim();
        if (t && dimensionMeta[d].validator(t)) activeFilters[d].add(t);
      });
    });
    if (Object.values(activeFilters).some(s => s.size > 0)) updateVisibleNodes();

    function syncFilterToURL() {
      const url = new URL(window.location);
      if (activeDim !== 'domains') url.searchParams.set('dim', activeDim);
      else url.searchParams.delete('dim');
      [['domains','domains'],['subgraphs','sub'],['tags','tags']].forEach(([d, key]) => {
        if (activeFilters[d].size > 0) {
          url.searchParams.set(key, Array.from(activeFilters[d]).join(','));
        } else {
          url.searchParams.delete(key);
        }
      });
      history.replaceState(null, '', url);
    }

    // Persist viewport in sessionStorage
    const VIEWPORT_KEY = 'graph-viewport';
    function saveViewport() {
      try {
        sessionStorage.setItem(VIEWPORT_KEY, JSON.stringify({
          x: transform.x, y: transform.y, k: transform.k
        }));
      } catch (e) {}
    }
    function loadViewport() {
      try {
        const raw = sessionStorage.getItem(VIEWPORT_KEY);
        if (raw) return JSON.parse(raw);
      } catch (e) {}
      return null;
    }

    function nodeHasValueInDim(n, dim) {
      if (dim === 'domains')   return n.domain != null && n.domain !== '';
      if (dim === 'subgraphs') return n.subgraph != null && n.subgraph !== '';
      if (dim === 'tags')      return (n.tags || []).length > 0;
      return false;
    }

    function updateVisibleNodes() {
      const anyActive = Object.values(activeFilters).some(s => s.size > 0);
      if (!anyActive) { visibleNodes = null; return; }
      visibleNodes = new Set();
      data.nodes.forEach(n => {
        if (activeFilters.domains.size > 0 && !activeFilters.domains.has(n.domain)) return;
        if (activeFilters.subgraphs.size > 0 && !activeFilters.subgraphs.has(n.subgraph)) return;
        if (activeFilters.tags.size > 0) {
          const tags = n.tags || [];
          let hit = false;
          for (const t of tags) { if (activeFilters.tags.has(t)) { hit = true; break; } }
          if (!hit) return;
        }
        visibleNodes.add(n.id);
      });
    }

    // Animate the canvas viewport to fit the currently filtered subset
    // (or the whole graph when no filter is active). Without this the
    // filter "happens" but isn't visually obvious — matching nodes can
    // be far apart under the existing zoom level.
    function fitToVisible() {
      if (!zoomBehavior) return;
      let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity, count = 0;
      // In tab-preview mode fit to the centroids of the active dim;
      // page nodes are dimmed so they shouldn't drive the framing.
      const points = (tabHighlight && groups[activeDim])
        ? Object.values(groups[activeDim])
        : data.nodes.filter(n => !visibleNodes || visibleNodes.has(n.id));
      for (const p of points) {
        if (typeof p.x !== 'number' || typeof p.y !== 'number') continue;
        if (p.x < minX) minX = p.x;
        if (p.x > maxX) maxX = p.x;
        if (p.y < minY) minY = p.y;
        if (p.y > maxY) maxY = p.y;
        count++;
      }
      if (count === 0 || !isFinite(minX)) return;
      const pad = 80;
      const bw = Math.max(1, maxX - minX);
      const bh = Math.max(1, maxY - minY);
      const k = Math.min((w - pad * 2) / bw, (h - pad * 2) / bh, 6);
      const cx = (minX + maxX) / 2;
      const cy = (minY + maxY) / 2;
      const tx = w / 2 - cx * k;
      const ty = h / 2 - cy * k;
      const next = d3.zoomIdentity.translate(tx, ty).scale(k);
      d3.select(canvas)
        .transition()
        .duration(450)
        .call(zoomBehavior.transform, next);
    }

    let pillsRowEl = null;
    let tabsEl = null;

    function buildFilterUI() {
      // Container with two rows: tabs (dimension switcher) + pills.
      const wrap = document.createElement('div');
      wrap.className = 'graph-filter';

      tabsEl = document.createElement('div');
      tabsEl.className = 'graph-filter-tabs';
      ['domains', 'subgraphs', 'tags'].forEach(d => {
        const meta = dimensionMeta[d];
        if (!meta.values.length) return;
        const btn = document.createElement('button');
        btn.className = 'graph-filter-tab';
        btn.dataset.dim = d;
        btn.addEventListener('click', () => {
          // Same tab clicked again → toggle preview off so the user
          // can interact with page nodes without first hunting for an
          // exit pill. Switching to a different tab → enter preview
          // unless that dim already has explicit pills selected.
          if (activeDim === d) {
            tabHighlight = !tabHighlight;
          } else {
            activeDim = d;
            tabHighlight = activeFilters[d].size === 0;
          }
          renderPillsRow();
          updateTabStates();
          syncFilterToURL();
          updateVisibleNodes();
          updateStats();
          draw();
          fitToVisible();
        });
        tabsEl.appendChild(btn);
      });
      wrap.appendChild(tabsEl);

      pillsRowEl = document.createElement('div');
      pillsRowEl.className = 'graph-filter-pills';
      wrap.appendChild(pillsRowEl);

      document.body.appendChild(wrap);

      renderPillsRow();
      updateTabStates();
    }

    function renderPillsRow() {
      if (!pillsRowEl) return;
      pillsRowEl.innerHTML = '';
      const meta = dimensionMeta[activeDim];
      if (!meta || !meta.values.length) return;
      const set = activeFilters[activeDim];

      const allPill = document.createElement('button');
      allPill.className = 'graph-filter-pill all-pill' + (set.size === 0 ? ' active' : '');
      allPill.textContent = 'all';
      allPill.addEventListener('click', () => {
        set.clear();
        // Clearing pills also clears any tab-preview highlight in
        // this dim, so the user gets back to a clean unrestricted view.
        tabHighlight = false;
        applyFilterChange();
      });
      pillsRowEl.appendChild(allPill);

      meta.values.forEach(v => {
        const pill = document.createElement('button');
        pill.className = 'graph-filter-pill' + (set.has(v) ? ' active' : '');
        pill.textContent = v;
        pill.dataset.value = v;
        pill.addEventListener('click', () => {
          if (set.has(v)) set.delete(v); else set.add(v);
          // Selecting concrete pills replaces the tab-preview state —
          // user has expressed a specific filter intent.
          tabHighlight = false;
          applyFilterChange();
        });
        // Pill hover = scrubbing preview. Mirrors the centroid-hover
        // path: build the member set for this value in the active dim
        // and let the draw loop lift those pages out of the dim sea.
        // Without this the tab-preview view felt static — you saw
        // centroids but had to click a pill to find out what was in one.
        pill.addEventListener('mouseenter', () => {
          if (!tabHighlight) return;
          const members = new Set();
          for (const n of data.nodes) {
            let match = false;
            if (activeDim === 'domains')   match = n.domain === v;
            else if (activeDim === 'subgraphs') match = n.subgraph === v;
            else if (activeDim === 'tags')      match = (n.tags || []).includes(v);
            if (match) members.add(n.id);
          }
          centroidMembers = members.size ? members : null;
          hoveredCentroid = v;
          draw();
        });
        pill.addEventListener('mouseleave', () => {
          if (!tabHighlight) return;
          if (hoveredCentroid === v) {
            centroidMembers = null;
            hoveredCentroid = null;
            draw();
          }
        });
        pillsRowEl.appendChild(pill);
      });
    }

    function applyFilterChange() {
      updateVisibleNodes();
      updatePillStates();
      updateTabStates();
      updateStats();
      syncFilterToURL();
      draw();
      fitToVisible();
    }

    function updatePillStates() {
      if (!pillsRowEl) return;
      const set = activeFilters[activeDim];
      pillsRowEl.querySelectorAll('.graph-filter-pill').forEach(pill => {
        const v = pill.dataset.value;
        if (!v) {
          pill.classList.toggle('active', set.size === 0);
        } else {
          pill.classList.toggle('active', set.has(v));
        }
      });
    }

    function updateTabStates() {
      if (!tabsEl) return;
      tabsEl.querySelectorAll('.graph-filter-tab').forEach(btn => {
        const d = btn.dataset.dim;
        const count = activeFilters[d].size;
        // "active" = this dim is actually doing something (preview
        // running, or pills selected). Just having activeDim point at
        // this tab — which is true on first paint — shouldn't make it
        // look engaged; that misleads the reader into thinking a
        // filter is on.
        const engaged = (d === activeDim && tabHighlight) || count > 0;
        btn.classList.toggle('active', engaged);
        btn.classList.toggle('current', d === activeDim);
        btn.classList.toggle('has-selection', count > 0);
        btn.textContent = count > 0
          ? dimensionMeta[d].label + ' · ' + count
          : dimensionMeta[d].label;
      });
    }

    function formatBytes(bytes) {
      if (bytes < 1024) return bytes + ' B';
      if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + ' KB';
      if (bytes < 1024 * 1024 * 1024) return (bytes / (1024 * 1024)).toFixed(1) + ' MB';
      return (bytes / (1024 * 1024 * 1024)).toFixed(2) + ' GB';
    }
    function formatLines(lines) {
      if (lines < 1000) return lines + ' loc';
      if (lines < 1_000_000) return (lines / 1000).toFixed(1) + 'k loc';
      return (lines / 1_000_000).toFixed(2) + 'M loc';
    }

    function updateStats() {
      if (!stats) return;
      const sizeStr = data.totalBytes != null ? ' &middot; ' + formatBytes(data.totalBytes) : '';
      const locStr = data.totalLines != null ? ' &middot; ' + formatLines(data.totalLines) : '';
      if (visibleNodes) {
        const visEdges = edgeRefs.filter(e => {
          const sid = e.source.id || e.source;
          const tid = e.target.id || e.target;
          return visibleNodes.has(sid) || visibleNodes.has(tid);
        }).length;
        stats.innerHTML = visibleNodes.size + ' / ' + data.nodes.length + ' files &middot; ' + visEdges + ' connections' + sizeStr + locStr;
      } else {
        stats.innerHTML = data.nodes.length + ' files &middot; ' + data.edges.length + ' connections' + sizeStr + locStr;
      }
    }

    // Check if positions are pre-computed (from build)
    const hasLayout = data.nodes.length > 0 && data.nodes.some(n => n.x !== 0 || n.y !== 0);

    if (hasLayout) {
      // Server-side force layout can produce multi-modal clusters where
      // subgraphs settle far from the main graph. Simple min-max would
      // then compress every dense region to a single dot. Use MAD-based
      // bounds: median ± k·MAD captures the dominant cluster tightly
      // while outlying clusters stay in-world but off-screen until the
      // reader pans or zooms out.
      function robustBounds(values) {
        const sorted = values.slice().sort((a, b) => a - b);
        const median = sorted[Math.floor(sorted.length / 2)];
        const devs = sorted.map(v => Math.abs(v - median)).sort((a, b) => a - b);
        const mad = devs[Math.floor(devs.length / 2)] || 1;
        const spread = Math.max(1, mad * 5);
        return { min: median - spread, max: median + spread, center: median };
      }
      const xs = data.nodes.map(d => d.x);
      const ys = data.nodes.map(d => d.y);
      const xb = robustBounds(xs);
      const yb = robustBounds(ys);
      const graphW = xb.max - xb.min || 1;
      const graphH = yb.max - yb.min || 1;
      const scale = Math.min(w / graphW, h / graphH) * 0.85;
      const cx = xb.center;
      const cy = yb.center;
      data.nodes.forEach(d => {
        d.x = w / 2 + (d.x - cx) * scale;
        d.y = h / 2 + (d.y - cy) * scale;
      });
      finishSetup();
    } else {
      // Fallback: compute layout in browser
      data.nodes.forEach(d => {
        const angle = Math.random() * Math.PI * 2;
        const r = Math.random() * Math.min(w, h) * 0.35;
        d.x = w / 2 + Math.cos(angle) * r;
        d.y = h / 2 + Math.sin(angle) * r;
      });

      const overlay = document.createElement('div');
      overlay.style.cssText = 'position:absolute;top:0;left:0;width:100%;height:100%;display:flex;flex-direction:column;align-items:center;justify-content:center;z-index:20;pointer-events:none';
      const progText = document.createElement('div');
      progText.style.cssText = 'color:var(--color-primary);font-size:14px;font-weight:500;letter-spacing:0.05em';
      progText.textContent = 'Computing layout\u2026';
      const progBar = document.createElement('div');
      progBar.style.cssText = 'width:200px;height:2px;background:color-mix(in srgb,var(--color-primary) 15%,transparent);border-radius:1px;margin-top:12px;overflow:hidden';
      const progFill = document.createElement('div');
      progFill.style.cssText = 'width:0%;height:100%;background:var(--color-primary);border-radius:1px;transition:width 0.1s';
      progBar.appendChild(progFill);
      overlay.appendChild(progText);
      overlay.appendChild(progBar);
      container.appendChild(overlay);

      const simulation = d3.forceSimulation(data.nodes)
        .force('link', d3.forceLink(data.edges).id(d => d.id).distance(20).strength(0.3))
        .force('charge', d3.forceManyBody().strength(-15).distanceMax(250).theta(0.9))
        .force('center', d3.forceCenter(w / 2, h / 2))
        .force('collision', d3.forceCollide().radius(d => radiusScale(d.focus) + 2).strength(0.8).iterations(1))
        .alphaDecay(0.04)
        .stop();

      const maxTicks = 300;
      const chunkSize = 15;
      let ticksDone = 0;
      const t0 = performance.now();

      function tickChunk() {
        for (let i = 0; i < chunkSize && ticksDone < maxTicks; i++, ticksDone++) {
          simulation.tick();
          if (simulation.alpha() < 0.001) { ticksDone = maxTicks; break; }
        }
        const pct = Math.min(100, Math.round(ticksDone / maxTicks * 100));
        progFill.style.width = pct + '%';
        progText.textContent = 'Computing layout\u2026 ' + pct + '%';

        if (ticksDone < maxTicks) {
          setTimeout(tickChunk, 0);
        } else {
          overlay.remove();
          finishSetup();
        }
      }
      tickChunk();
    }

    function finishSetup() {
      // Build edge source/target refs (d3 replaces IDs with objects after simulation)
      edgeRefs = data.edges.map(e => ({
        source: typeof e.source === 'object' ? e.source : nodeById[e.source],
        target: typeof e.target === 'object' ? e.target : nodeById[e.target]
      }));

      // Compute centroids per dim value (uses post-normalization
      // node positions). Tags are restricted to the same top-N
      // tagCounts list shown in pills.
      function bumpGroup(map, key, n) {
        if (!map[key]) map[key] = { sumX: 0, sumY: 0, count: 0, value: key };
        map[key].sumX += n.x;
        map[key].sumY += n.y;
        map[key].count += 1;
      }
      for (const n of data.nodes) {
        if (typeof n.x !== 'number' || typeof n.y !== 'number') continue;
        if (n.domain) bumpGroup(groups.domains, n.domain, n);
        if (n.subgraph) bumpGroup(groups.subgraphs, n.subgraph, n);
        for (const t of (n.tags || [])) {
          if (tagCounts[t] >= 3) bumpGroup(groups.tags, t, n);
        }
      }
      for (const dim of ['domains', 'subgraphs', 'tags']) {
        for (const k in groups[dim]) {
          const g = groups[dim][k];
          g.x = g.sumX / g.count;
          g.y = g.sumY / g.count;
        }
      }

      buildFilterUI();
      updatePillStates();
      updateStats();
      draw();

      // Enable zoom + interactions
      zoomBehavior = d3.zoom()
        .scaleExtent([0.1, 20])
        .on('zoom', (event) => {
          transform = event.transform;
          saveViewport();
          draw();
        });
      d3.select(canvas).call(zoomBehavior);

      // Restore saved viewport
      const saved = loadViewport();
      if (saved) {
        transform = d3.zoomIdentity.translate(saved.x, saved.y).scale(saved.k);
        d3.select(canvas).call(zoomBehavior.transform, transform);
      }

      setupInteractions();
    }

    function draw() {
      ctx.save();
      ctx.clearRect(0, 0, w, h);
      ctx.translate(transform.x, transform.y);
      ctx.scale(transform.k, transform.k);

      const k = transform.k;
      const connectedSet = hoveredNode ? (adj[hoveredNode.id] || new Set()) : null;

      // Determine if a node is visible (passes tag filter)
      const isVisible = visibleNodes ? (id) => visibleNodes.has(id) : () => true;

      // Draw edges — when hovering, when filter active, or when a
      // centroid is hovered in tab preview (show connections between
      // its members).
      if (hoveredNode || visibleNodes || centroidMembers) {
        ctx.strokeStyle = colorPrimary;
        ctx.lineWidth = 0.8 / k;
        ctx.beginPath();
        for (const e of edgeRefs) {
          const sid = e.source.id || e.source;
          const tid = e.target.id || e.target;
          let show = false;
          if (hoveredNode && (sid === hoveredNode.id || tid === hoveredNode.id)) {
            show = true;
          } else if (centroidMembers && centroidMembers.has(sid) && centroidMembers.has(tid)) {
            show = true;
          } else if (visibleNodes && isVisible(sid) && isVisible(tid)) {
            show = true;
          }
          if (show) {
            ctx.moveTo(e.source.x, e.source.y);
            ctx.lineTo(e.target.x, e.target.y);
          }
        }
        ctx.globalAlpha = centroidMembers ? 0.4 : (visibleNodes ? 0.15 : 0.3);
        ctx.stroke();
        ctx.globalAlpha = 1;
      }

      // Draw nodes
      for (const n of data.nodes) {
        const r = radiusScale(n.focus);
        const nodeVisible = isVisible(n.id);
        let alpha = opacityScale(n.focus);

        // Tag filter fading
        if (visibleNodes && !nodeVisible) {
          alpha = 0.02;
        }

        // Tab preview: gray out the page-node sea so centroid
        // super-nodes (drawn below) carry the eye. When a centroid
        // is hovered, lift its members back to full visibility so
        // the user sees which pages belong to that domain/subgraph/tag.
        if (tabHighlight) {
          if (centroidMembers && centroidMembers.has(n.id)) {
            alpha = Math.max(alpha, 0.85);
          } else {
            alpha = Math.min(alpha, 0.05);
          }
        }

        if (hoveredNode) {
          if (n.id === hoveredNode.id) {
            alpha = 1;
          } else if (connectedSet.has(n.id)) {
            alpha = visibleNodes && !nodeVisible ? 0.15 : 0.7;
          } else {
            alpha = 0.03;
          }
        }

        ctx.beginPath();
        ctx.arc(n.x, n.y, r, 0, Math.PI * 2);
        ctx.fillStyle = colorPrimary;
        ctx.globalAlpha = alpha;
        ctx.fill();

        // Glow on hovered node
        if (hoveredNode && n.id === hoveredNode.id) {
          ctx.shadowColor = colorPrimary;
          ctx.shadowBlur = 12;
          ctx.strokeStyle = colorPrimary;
          ctx.lineWidth = 1.5 / k;
          ctx.stroke();
          ctx.shadowBlur = 0;
        }

        ctx.globalAlpha = 1;
      }

      // Draw labels — above nodes, clean
      const showLabelsAtZoom = k >= 0.5;
      if (showLabelsAtZoom) {
        ctx.textAlign = 'center';
        ctx.textBaseline = 'bottom';

        for (const n of data.nodes) {
          const r = radiusScale(n.focus);
          let show = false;
          let alpha = 0.7;

          const nodeVisible = isVisible(n.id);

          if (hoveredNode) {
            if (n.id === hoveredNode.id || connectedSet.has(n.id)) {
              show = true;
              alpha = n.id === hoveredNode.id ? 1 : 0.75;
            } else if (labelSet.has(n.id)) {
              show = true;
              alpha = 0.15;
            }
          } else if (centroidMembers) {
            // Centroid hovered: label every member of that group.
            if (centroidMembers.has(n.id)) {
              show = true;
              alpha = 0.85;
            }
          } else if (tabHighlight) {
            // Tab preview, nothing hovered: page nodes are dimmed to
            // 0.05, so their labels would be a wall of unrelated text.
            // Let the centroid super-nodes carry the eye instead.
            show = false;
          } else if (visibleNodes) {
            // Filter active: label all visible nodes
            if (nodeVisible) {
              show = true;
              alpha = 0.8;
            }
          } else {
            // Default: show top labels, more at higher zoom
            if (labelSet.has(n.id)) {
              show = true;
              alpha = 0.7;
            } else if (k >= 2 && n.focus > maxFocus * 0.08) {
              show = true;
              alpha = 0.5;
            } else if (k >= 4 && n.focus > maxFocus * 0.02) {
              show = true;
              alpha = 0.4;
            } else if (k >= 6) {
              show = true;
              alpha = 0.35;
            }
          }

          if (!show) continue;

          const fontSize = Math.max(9, Math.min(14, 10 + r * 0.2)) / k;
          ctx.font = '500 ' + fontSize + 'px system-ui, sans-serif';
          ctx.globalAlpha = alpha;

          // Text shadow for readability
          ctx.strokeStyle = colorBg;
          ctx.lineWidth = 3 / k;
          ctx.lineJoin = 'round';
          ctx.strokeText(n.title, n.x, n.y - r - 3 / k);

          ctx.fillStyle = colorText;
          ctx.fillText(n.title, n.x, n.y - r - 3 / k);
        }
      }

      // Tab preview: render labeled super-nodes for each value in the
      // active dimension, positioned at the centroid of its pages.
      // Drawn after nodes/labels so they sit on top.
      if (tabHighlight) {
        const dimGroups = groups[activeDim] || {};
        const groupArr = Object.values(dimGroups);
        if (groupArr.length) {
          const maxCount = groupArr.reduce((m, g) => Math.max(m, g.count), 1);
          for (const g of groupArr) {
            const r = (10 + Math.sqrt(g.count / maxCount) * 30) / k;
            ctx.beginPath();
            ctx.arc(g.x, g.y, r, 0, Math.PI * 2);
            ctx.fillStyle = colorPrimary;
            ctx.globalAlpha = 0.85;
            ctx.fill();
            ctx.shadowColor = colorPrimary;
            ctx.shadowBlur = 14 / k;
            ctx.lineWidth = 1.5 / k;
            ctx.strokeStyle = colorPrimary;
            ctx.stroke();
            ctx.shadowBlur = 0;
          }
          // Labels above each super-node
          ctx.textAlign = 'center';
          ctx.textBaseline = 'bottom';
          for (const g of groupArr) {
            const r = (10 + Math.sqrt(g.count / maxCount) * 30) / k;
            const fontSize = Math.max(11, Math.min(18, 12 + r * 0.3 * k)) / k;
            ctx.font = '600 ' + fontSize + 'px system-ui, sans-serif';
            ctx.lineWidth = 4 / k;
            ctx.lineJoin = 'round';
            ctx.strokeStyle = colorBg;
            ctx.globalAlpha = 1;
            ctx.strokeText(g.value, g.x, g.y - r - 4 / k);
            ctx.fillStyle = colorText;
            ctx.fillText(g.value, g.x, g.y - r - 4 / k);
          }
        }
      }

      ctx.globalAlpha = 1;
      ctx.restore();
    }

    // Hit detection — prefer visible nodes when filter active
    function findNode(mx, my) {
      const gx = (mx - transform.x) / transform.k;
      const gy = (my - transform.y) / transform.k;
      let closest = null;
      let closestDist = Infinity;
      for (const n of data.nodes) {
        if (visibleNodes && !visibleNodes.has(n.id)) continue;
        const r = radiusScale(n.focus);
        const hitR = Math.max(r, 5 / transform.k);
        const dx = gx - n.x;
        const dy = gy - n.y;
        const dist = dx * dx + dy * dy;
        if (dist < hitR * hitR && dist < closestDist) {
          closest = n;
          closestDist = dist;
        }
      }
      return closest;
    }

    // Centroid hit detection (used during tab preview). Matches the
    // sizing logic in draw() so the visible super-node is the actual
    // hit target.
    function findCentroid(mx, my) {
      if (!tabHighlight) return null;
      const dimGroups = groups[activeDim] || {};
      const groupArr = Object.values(dimGroups);
      if (!groupArr.length) return null;
      const maxCount = groupArr.reduce((m, g) => Math.max(m, g.count), 1);
      const gx = (mx - transform.x) / transform.k;
      const gy = (my - transform.y) / transform.k;
      let closest = null;
      let closestDist = Infinity;
      for (const g of groupArr) {
        const r = (10 + Math.sqrt(g.count / maxCount) * 30) / transform.k;
        const hitR = Math.max(r, 6 / transform.k);
        const dx = gx - g.x;
        const dy = gy - g.y;
        const dist = dx * dx + dy * dy;
        if (dist < hitR * hitR && dist < closestDist) {
          closest = g;
          closestDist = dist;
        }
      }
      return closest;
    }

    // Tooltip
    const tooltip = document.createElement('div');
    tooltip.style.cssText = 'position:absolute;display:none;background:color-mix(in srgb,var(--color-bg) 85%,transparent);backdrop-filter:blur(8px);-webkit-backdrop-filter:blur(8px);border:1px solid var(--color-primary);padding:8px 14px;border-radius:8px;font-size:13px;pointer-events:none;box-shadow:0 0 20px rgba(34,197,94,0.15);z-index:10';
    container.appendChild(tooltip);

    let dragNode = null;
    let isDragging = false;

    function setupInteractions() {
      canvas.addEventListener('mousemove', (event) => {
        if (dragNode) {
          isDragging = true;
          const rect = canvas.getBoundingClientRect();
          dragNode.x = (event.clientX - rect.left - transform.x) / transform.k;
          dragNode.y = (event.clientY - rect.top - transform.y) / transform.k;
          draw();
          return;
        }
        const rect = canvas.getBoundingClientRect();
        const mx = event.clientX - rect.left;
        const my = event.clientY - rect.top;

        // Centroid takes precedence in tab-preview mode; otherwise
        // fall through to standard page-node hover so the graph
        // behaves the same way regardless of which dimension tab is
        // active.
        const centroid = tabHighlight ? findCentroid(mx, my) : null;
        const node = centroid ? null : findNode(mx, my);

        // Update hoveredCentroid + its member set (used by draw to
        // light up just this group's pages and their edges).
        if (centroid !== hoveredCentroid) {
          hoveredCentroid = centroid;
          centroidMembers = null;
          if (centroid) {
            centroidMembers = new Set();
            for (const n of data.nodes) {
              let match = false;
              if (activeDim === 'domains')   match = n.domain === centroid.value;
              else if (activeDim === 'subgraphs') match = n.subgraph === centroid.value;
              else if (activeDim === 'tags')      match = (n.tags || []).indexOf(centroid.value) !== -1;
              if (match) centroidMembers.add(n.id);
            }
          }
          draw();
        }

        if (node !== hoveredNode) {
          hoveredNode = node;
          draw();
        }

        if (centroid) {
          canvas.style.cursor = 'pointer';
          tooltip.style.display = 'block';
          tooltip.innerHTML = '<strong style="font-size:15px">' + centroid.value + '</strong><br><span style="font-size:12px;opacity:0.6">' + centroid.count + ' pages</span>';
          tooltip.style.left = (event.clientX - rect.left + 15) + 'px';
          tooltip.style.top = (event.clientY - rect.top - 10) + 'px';
        } else if (node) {
          canvas.style.cursor = 'pointer';
          tooltip.style.display = 'block';
          tooltip.innerHTML = '<strong style="font-size:15px">' + node.title + '</strong><br><span style="font-size:12px;opacity:0.6">π ' + ((node.focus || 0) * 100).toFixed(2) + '% · ' + node.linkCount + ' links</span>';
          tooltip.style.left = (event.clientX - rect.left + 15) + 'px';
          tooltip.style.top = (event.clientY - rect.top - 10) + 'px';
        } else {
          canvas.style.cursor = 'grab';
          tooltip.style.display = 'none';
        }
      });

      canvas.addEventListener('mouseleave', () => {
        hoveredNode = null;
        hoveredCentroid = null;
        centroidMembers = null;
        tooltip.style.display = 'none';
        draw();
      });

      canvas.addEventListener('click', (event) => {
        if (isDragging) return;
        const rect = canvas.getBoundingClientRect();
        const mx = event.clientX - rect.left;
        const my = event.clientY - rect.top;

        // Centroid click drills into that value (in tab preview);
        // page-node click navigates as usual; empty-area click in
        // preview exits preview.
        if (tabHighlight) {
          const c = findCentroid(mx, my);
          if (c) {
            activeFilters[activeDim].add(c.value);
            tabHighlight = false;
            applyFilterChange();
            renderPillsRow();
            return;
          }
        }

        const node = findNode(mx, my);
        if (node) {
          window.location.href = node.url;
          return;
        }

        if (tabHighlight) {
          tabHighlight = false;
          updateVisibleNodes();
          updateTabStates();
          updateStats();
          draw();
        }
      });

      canvas.addEventListener('mousedown', (event) => {
        const rect = canvas.getBoundingClientRect();
        const node = findNode(event.clientX - rect.left, event.clientY - rect.top);
        if (node) {
          dragNode = node;
          isDragging = false;
        }
      });

      canvas.addEventListener('mouseup', () => {
        dragNode = null;
        isDragging = false;
      });

      window.addEventListener('resize', () => {
        const nw = container.clientWidth || window.innerWidth;
        const nh = container.clientHeight || window.innerHeight;
        canvas.width = nw * dpr;
        canvas.height = nh * dpr;
        canvas.style.width = nw + 'px';
        canvas.style.height = nh + 'px';
        ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
        draw();
      });
    }

    // Stats (appended early so filter UI can update it)
    updateStats();
    container.appendChild(stats);
  }
})();

// Minimap widget for per-page graph
(function() {
  const container = document.getElementById('minimap-container');
  if (!container) return;

  const pageId = container.dataset.pageId;
  const depth = parseInt(container.dataset.depth) || 2;
  const maxNodes = parseInt(container.dataset.maxNodes) || 30;
  if (!pageId) return;

  function loadAndRender() {
    fetch('/graph-data.json')
      .then(r => r.json())
      .then(fullGraph => {
        const data = extractNeighborhood(fullGraph, pageId, depth, maxNodes);
        if (data.nodes.length > 1) {
          renderMinimap(data);
        } else {
          container.innerHTML = '<p style="color:#888;font-size:12px">No connections</p>';
        }
      })
      .catch(() => {});
  }

  function extractNeighborhood(graph, centerId, maxDepth, maxNodes) {
    const adj = {};
    const nodeMap = {};
    graph.nodes.forEach(n => { nodeMap[n.id] = n; adj[n.id] = []; });
    graph.edges.forEach(e => {
      const s = typeof e.source === 'object' ? e.source.id : e.source;
      const t = typeof e.target === 'object' ? e.target.id : e.target;
      if (adj[s]) adj[s].push(t);
      if (adj[t]) adj[t].push(s);
    });

    const hop = { [centerId]: 0 };
    let frontier = [centerId];
    for (let d = 0; d < maxDepth; d++) {
      const next = [];
      for (const nid of frontier) {
        for (const neighbor of (adj[nid] || [])) {
          if (!(neighbor in hop)) {
            hop[neighbor] = d + 1;
            next.push(neighbor);
          }
        }
      }
      frontier = next;
    }

    const score = id => {
      const n = nodeMap[id] || {};
      const focus = typeof n.focus === 'number' ? n.focus : 0;
      const rank = typeof n.pageRank === 'number' ? n.pageRank : 0;
      return (focus + rank) / (1 + (hop[id] || 0));
    };

    const ranked = Object.keys(hop)
      .filter(id => nodeMap[id] && id !== centerId)
      .sort((a, b) => score(b) - score(a))
      .slice(0, Math.max(0, maxNodes - 1));
    const keep = new Set([centerId, ...ranked]);

    const nodes = Array.from(keep).map(id => {
      const n = nodeMap[id];
      return {
        id,
        title: n.title,
        url: n.url,
        current: id === centerId,
        focus: typeof n.focus === 'number' ? n.focus : 0,
        pageRank: typeof n.pageRank === 'number' ? n.pageRank : 0,
        hop: hop[id] || 0,
      };
    });
    const edges = graph.edges.filter(e => {
      const s = typeof e.source === 'object' ? e.source.id : e.source;
      const t = typeof e.target === 'object' ? e.target.id : e.target;
      return keep.has(s) && keep.has(t);
    }).map(e => ({
      source: typeof e.source === 'object' ? e.source.id : e.source,
      target: typeof e.target === 'object' ? e.target.id : e.target
    }));
    return { nodes, edges };
  }

  // Defer the 8.7 MB graph-data.json fetch until the user actually
  // scrolls the minimap into view. Eager-loading on every page hit
  // forced the browser to revalidate + parse 8.7 MB of JSON on each
  // navigation; rapid clicks then queued duplicate parses behind one
  // another and the next click felt stuck.
  let booted = false;
  function boot() {
    if (booted) return;
    booted = true;
    if (window.d3) {
      loadAndRender();
    } else {
      const script = document.createElement('script');
      script.src = 'https://d3js.org/d3.v7.min.js';
      script.onload = loadAndRender;
      document.head.appendChild(script);
    }
  }
  if ('IntersectionObserver' in window) {
    const io = new IntersectionObserver(function (entries) {
      for (const e of entries) {
        if (e.isIntersecting) { io.disconnect(); boot(); break; }
      }
    }, { rootMargin: '200px' });
    io.observe(container);
  } else {
    // Fallback: legacy browsers — load after first idle moment.
    (window.requestIdleCallback || setTimeout)(boot, 1500);
  }

  function renderMinimap(data) {
    const w = container.clientWidth || 720;
    const h = 460;
    const pad = 36;

    // Node radius driven by focus (pi share) with pageRank as a gentle
    // fallback; current page always largest so the eye lands on it.
    const maxFocus = Math.max(0.0001, ...data.nodes.map(n => n.focus || 0));
    const radius = d => {
      if (d.current) return 11;
      const f = (d.focus || d.pageRank || 0) / maxFocus;
      return 4 + Math.sqrt(f) * 6; // 4 .. 10
    };

    const svg = d3.select(container)
      .append('svg')
      .attr('width', w)
      .attr('height', h)
      .attr('viewBox', [0, 0, w, h])
      .style('display', 'block')
      .style('max-width', '100%');

    // Adjacency for hover emphasis.
    const neighbors = {};
    data.nodes.forEach(n => { neighbors[n.id] = new Set([n.id]); });
    data.edges.forEach(e => {
      const s = typeof e.source === 'object' ? e.source.id : e.source;
      const t = typeof e.target === 'object' ? e.target.id : e.target;
      neighbors[s] && neighbors[s].add(t);
      neighbors[t] && neighbors[t].add(s);
    });

    const g = svg.append('g');

    const simulation = d3.forceSimulation(data.nodes)
      .force('link', d3.forceLink(data.edges).id(d => d.id).distance(90).strength(0.55))
      .force('charge', d3.forceManyBody().strength(-320))
      .force('x', d3.forceX(w / 2).strength(0.04))
      .force('y', d3.forceY(h / 2).strength(0.04))
      .force('collision', d3.forceCollide(d => radius(d) + 6));

    // Compute scale + translation so the node bbox fills the viewport on
    // every tick. Without this, even strong repulsion leaves a tight
    // cluster in the middle; with it the graph always uses all the space.
    function fit() {
      if (!data.nodes.length) return { scale: 1, tx: 0, ty: 0 };
      let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
      for (const n of data.nodes) {
        if (typeof n.x !== 'number' || typeof n.y !== 'number') continue;
        if (n.x < minX) minX = n.x;
        if (n.x > maxX) maxX = n.x;
        if (n.y < minY) minY = n.y;
        if (n.y > maxY) maxY = n.y;
      }
      if (!isFinite(minX)) return { scale: 1, tx: 0, ty: 0 };
      const bw = Math.max(1, maxX - minX);
      const bh = Math.max(1, maxY - minY);
      const scale = Math.min((w - pad * 2) / bw, (h - pad * 2) / bh, 3.5);
      return {
        scale,
        tx: (w - bw * scale) / 2 - minX * scale,
        ty: (h - bh * scale) / 2 - minY * scale,
      };
    }
    let current = fit();

    const link = g.append('g')
      .attr('stroke', 'var(--color-text)')
      .attr('stroke-opacity', 0.18)
      .attr('stroke-width', 1)
      .selectAll('line')
      .data(data.edges)
      .join('line');

    const node = g.append('g')
      .selectAll('circle')
      .data(data.nodes)
      .join('circle')
      .attr('r', radius)
      .attr('fill', d => d.current ? 'var(--color-primary)' : 'var(--color-secondary)')
      .attr('fill-opacity', d => d.current ? 1 : 0.8)
      .attr('stroke', d => d.current ? 'var(--color-primary)' : 'transparent')
      .attr('stroke-width', d => d.current ? 2 : 0)
      .attr('stroke-opacity', 0.4)
      .style('cursor', 'pointer')
      .on('click', (event, d) => { window.location.href = d.url; })
      .call(d3.drag()
        .on('start', (event, d) => {
          if (!event.active) simulation.alphaTarget(0.3).restart();
          d.fx = d.x; d.fy = d.y;
        })
        .on('drag', (event, d) => {
          // event.x/y are in SVG coords; simulation works in its own
          // frame, so reverse the current fit transform before pinning.
          d.fx = (event.x - current.tx) / current.scale;
          d.fy = (event.y - current.ty) / current.scale;
        })
        .on('end', (event, d) => {
          if (!event.active) simulation.alphaTarget(0);
          d.fx = null; d.fy = null;
        }));

    const label = g.append('g')
      .attr('font-family', 'inherit')
      .attr('font-size', '11px')
      .attr('fill', 'var(--color-text)')
      .attr('text-anchor', 'middle')
      .style('paint-order', 'stroke')
      .attr('stroke', 'var(--color-bg)')
      .attr('stroke-width', 3)
      .attr('stroke-linejoin', 'round')
      .selectAll('text')
      .data(data.nodes)
      .join('text')
      .text(d => d.title.length > 22 ? d.title.slice(0, 20) + '\u2026' : d.title)
      .attr('font-weight', d => d.current ? 700 : 400)
      .style('pointer-events', 'none');

    // Hover: highlight node, its neighbors, and the edges between. Dim rest.
    function onEnter(event, d) {
      const near = neighbors[d.id];
      node.attr('fill-opacity', n => near.has(n.id) ? 1 : 0.15)
          .attr('stroke', n => n.id === d.id ? 'var(--color-primary)' : (n.current ? 'var(--color-primary)' : 'transparent'))
          .attr('stroke-width', n => n.id === d.id ? 2 : (n.current ? 2 : 0))
          .attr('stroke-opacity', n => n.id === d.id ? 0.9 : 0.4);
      link.attr('stroke-opacity', e => {
        const s = typeof e.source === 'object' ? e.source.id : e.source;
        const t = typeof e.target === 'object' ? e.target.id : e.target;
        return (s === d.id || t === d.id) ? 0.6 : 0.05;
      });
      label.attr('fill-opacity', n => near.has(n.id) ? 1 : 0.25);
    }
    function onLeave() {
      node.attr('fill-opacity', d => d.current ? 1 : 0.8)
          .attr('stroke', d => d.current ? 'var(--color-primary)' : 'transparent')
          .attr('stroke-width', d => d.current ? 2 : 0);
      link.attr('stroke-opacity', 0.18);
      label.attr('fill-opacity', 1);
    }
    node.on('mouseenter', onEnter).on('mouseleave', onLeave);

    simulation.on('tick', () => {
      current = fit();
      const s = current.scale, tx = current.tx, ty = current.ty;
      link
        .attr('x1', d => d.source.x * s + tx)
        .attr('y1', d => d.source.y * s + ty)
        .attr('x2', d => d.target.x * s + tx)
        .attr('y2', d => d.target.y * s + ty);
      node
        .attr('cx', d => d.x * s + tx)
        .attr('cy', d => d.y * s + ty);
      label
        .attr('x', d => d.x * s + tx)
        .attr('y', d => d.y * s + ty - radius(d) - 4);
    });
  }
})();
