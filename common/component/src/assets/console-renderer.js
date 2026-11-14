/* Lan Studio capability renderer.
 * One source file is embedded into every component console and into the Shell
 * SPA. The widget registry is a complete set, but ONLY modules present in the
 * backend capability manifest are instantiated: an unreferenced module creates
 * no DOM node at all. Zero external resources, no framework, no build step.
 */
(function () {
  var ICONS = {
    grid: '<rect x="3" y="3" width="7.5" height="7.5" rx="2"/><rect x="13.5" y="3" width="7.5" height="7.5" rx="2"/><rect x="3" y="13.5" width="7.5" height="7.5" rx="2"/><rect x="13.5" y="13.5" width="7.5" height="7.5" rx="2"/>',
    sliders: '<path d="M4 7h9M17 7h3M4 17h3M11 17h9"/><circle cx="15" cy="7" r="2"/><circle cx="9" cy="17" r="2"/>',
    activity: '<path d="M3 12h4l2.5 7L14 4l2.5 8H21"/>',
    terminal: '<rect x="3" y="4" width="18" height="16" rx="3"/><path d="m7 9 3 3-3 3M12.5 15h4.5"/>',
    bell: '<path d="M18 9a6 6 0 0 0-12 0c0 5-2 6-2 6h16s-2-1-2-6M10 20a2 2 0 0 0 4 0"/>',
    history: '<path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5M12 7v5l3.5 2"/>',
    dns: '<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3a15 15 0 0 1 0 18M12 3a15 15 0 0 0 0 18"/>',
    relay: '<path d="M4 7h6l4 10h6M4 17h6l4-10h6"/><circle cx="4" cy="7" r="1.5"/><circle cx="20" cy="17" r="1.5"/>',
    cert: '<path d="M6 3h9l4 4v14H6z"/><path d="M15 3v5h4M9 13h7M9 17h5"/>',
    scan: '<path d="M3 8V5a2 2 0 0 1 2-2h3M16 3h3a2 2 0 0 1 2 2v3M21 16v3a2 2 0 0 1-2 2h-3M8 21H5a2 2 0 0 1-2-2v-3M3 12h18"/>',
    hosts: '<path d="M4 5h16v14H4zM4 10h16M8 5v14"/><path d="M11 14h5M11 17h3"/>',
    power: '<path d="M12 3v9M6.5 6.5a8 8 0 1 0 11 0"/>',
    sync: '<path d="M4 12a8 8 0 0 1 13.7-5.7L20 8M20 4v4h-4M20 12a8 8 0 0 1-13.7 5.7L4 16M4 20v-4h4"/>',
    discover: '<circle cx="11" cy="11" r="7"/><path d="M16 16l5 5M8 11h6M11 8v6"/>',
    cube: '<path d="M12 3l8 4.5v9L12 21l-8-4.5v-9zM4 7.5l8 4.5 8-4.5M12 12v9"/>'
  };
  var CORE_PAGE = {
    'core.status': 'overview', 'core.config': 'config', 'core.observe': 'observe',
    'core.logs': 'logs', 'core.alerts': 'alerts', 'core.history': 'history'
  };
  var CSS = [
    '.lc-panel{background:var(--surface,#fff);border:1px solid var(--line,#e7e9ee);border-radius:var(--radius,10px);padding:16px 18px;margin-bottom:16px;box-shadow:var(--shadow,0 1px 2px rgba(16,24,40,.04))}',
    '.lc-title{display:flex;align-items:center;gap:9px;font-size:14px;font-weight:700;margin-bottom:12px;color:var(--ink,#1d2430)}',
    '.lc-title svg{width:17px;height:17px;flex:none}',
    '.lc-hint{margin-left:auto;font-size:11.5px;font-weight:400;color:var(--ink3,#8b94a3)}',
    '.lc-grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(170px,1fr));gap:10px}',
    '.lc-cell{border:1px solid var(--line-soft,#eef0f4);border-radius:8px;padding:10px 12px;background:var(--tile,#f7f8fa)}',
    '.lc-cell b{display:block;font-size:11px;font-weight:500;color:var(--ink2,#5a6473);margin-bottom:4px}',
    '.lc-cell span{font-size:15px;font-weight:700;word-break:break-all;color:var(--ink,#1d2430)}',
    '.lc-list{border:1px solid var(--line-soft,#eef0f4);border-radius:8px;overflow:hidden}',
    '.lc-row{display:grid;gap:8px;padding:9px 12px;border-bottom:1px solid var(--line-soft,#eef0f4);font-size:12.5px;color:var(--ink,#1d2430)}',
    '.lc-row:last-child{border-bottom:0}.lc-row.lc-head{background:var(--tile,#f7f8fa);font-weight:700;color:var(--ink2,#5a6473)}',
    '.lc-code{width:100%;min-height:200px;background:var(--field,#fff);border:1px solid var(--field-border,#d8dce4);border-radius:8px;padding:10px 12px;font:12px/1.6 ui-monospace,SFMono-Regular,Menlo,monospace;white-space:pre-wrap;word-break:break-all;color:var(--ink,#1d2430)}',
    '.lc-bar{display:flex;flex-wrap:wrap;gap:8px;margin-top:12px}',
    '.lc-btn{border:1px solid var(--line,#e7e9ee);background:var(--surface,#fff);color:var(--ink,#1d2430);border-radius:8px;padding:7px 13px;cursor:pointer;font-size:12.5px;font-weight:600}',
    '.lc-btn:hover{border-color:var(--accent,#3b5bdb)}.lc-btn.primary{background:var(--accent,#3b5bdb);border-color:transparent;color:#fff}',
    '.lc-form{display:grid;grid-template-columns:110px 1fr;gap:10px 12px;margin-top:12px;padding:14px;border:1px dashed var(--line,#e7e9ee);border-radius:8px}',
    '.lc-form label{font-size:12.5px;color:var(--ink2,#5a6473);align-self:center}',
    '.lc-form input,.lc-form textarea{width:100%;background:var(--field,#fff);border:1px solid var(--field-border,#d8dce4);border-radius:8px;padding:7px 10px;font:inherit;color:var(--ink,#1d2430)}',
    '.lc-form .lc-wide{grid-column:1 / -1}.lc-form .lc-wide textarea{min-height:90px}',
    '.lc-empty{padding:14px;font-size:12.5px;color:var(--ink3,#8b94a3)}',
    '.lc-log{font:12px/1.7 ui-monospace,SFMono-Regular,Menlo,monospace;color:var(--ink,#1d2430)}',
    '.lc-log div{border-bottom:1px solid var(--line-soft,#eef0f4);padding:5px 2px;word-break:break-all}',
    '.lc-nav-label{padding:10px 10px 6px;font-size:10.5px;font-weight:700;letter-spacing:.14em;text-transform:uppercase;color:var(--ink3,#8b94a3)}'
  ].join('');

  function esc(v) {
    return String(v === undefined || v === null ? '' : v).replace(/[&<>"']/g, function (c) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c];
    });
  }
  function ce(tag) { return document.createElement(tag); }
  function svg(name) {
    return '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">' + (ICONS[name] || ICONS.cube) + '</svg>';
  }
  function ensureStyle() {
    if (document.getElementById('lc-style')) return;
    var s = ce('style'); s.id = 'lc-style'; s.textContent = CSS; document.head.appendChild(s);
  }

  function makeApi(baseFor, getPassword, extraHeaders) {
    return function (method, path, write, body) {
      var headers = { 'Content-Type': 'application/json' };
      var pwd = getPassword && getPassword();
      if (write && pwd) headers['X-Admin-Password'] = pwd;
      var extra = extraHeaders && extraHeaders() || {};
      Object.keys(extra).forEach(function (k) { headers[k] = extra[k]; });
      return fetch(baseFor(path), {
        method: method, headers: headers,
        body: body === undefined ? undefined : JSON.stringify(body)
      }).then(function (r) {
        var ok = r.ok;
        return r.json().catch(function () { return { ok: false, error: { message: 'invalid response' } }; })
          .then(function (j) {
            if (!ok || j.ok === false) {
              throw new Error((j.error && (j.error.code + ': ' + j.error.message)) || ('HTTP ' + r.status));
            }
            return j.data;
          });
      });
    };
  }

  function rowsOf(data) {
    if (Array.isArray(data)) return data;
    if (!data || typeof data !== 'object') return [];
    var keys = ['items', 'records', 'devices', 'nodes', 'mappings', 'certificates',
      'logs', 'entries', 'results', 'history', 'services', 'targets', 'interfaces', 'rows'];
    for (var i = 0; i < keys.length; i++) {
      if (Array.isArray(data[keys[i]])) return data[keys[i]];
    }
    return [];
  }
  function textOf(data) {
    if (typeof data === 'string') return data;
    if (!data || typeof data !== 'object') return String(data);
    return data.raw || data.text || data.config_text || data.rendered || JSON.stringify(data, null, 2);
  }

  var WIDGETS = {
    'kv-grid': function (box, panel, data) {
      var obj = data && typeof data === 'object' && !Array.isArray(data) ? data : { value: textOf(data) };
      var grid = ce('div'); grid.className = 'lc-grid';
      Object.keys(obj).forEach(function (k) {
        var v = obj[k];
        if (v && typeof v === 'object') return;
        var cell = ce('div'); cell.className = 'lc-cell';
        cell.innerHTML = '<b>' + esc(k.replace(/_/g, ' ')) + '</b><span>' + esc(v === true ? '是' : v === false ? '否' : v) + '</span>';
        grid.appendChild(cell);
      });
      box.appendChild(grid);
    },
    'metric-grid': function (box, panel, data) { WIDGETS['kv-grid'](box, panel, data); },
    'table': function (box, panel, data) {
      var rows = rowsOf(data);
      if (!rows.length) { var e = ce('div'); e.className = 'lc-empty'; e.textContent = '暂无数据'; box.appendChild(e); return; }
      var cols = panel.columns && panel.columns.length ? panel.columns :
        Object.keys(rows[0]).slice(0, 6).map(function (k) { return { key: k, label: k }; });
      var list = ce('div'); list.className = 'lc-list';
      var head = ce('div'); head.className = 'lc-row lc-head';
      head.style.gridTemplateColumns = 'repeat(' + cols.length + ',minmax(0,1fr))';
      head.innerHTML = cols.map(function (c) { return '<span>' + esc(c.label) + '</span>'; }).join('');
      list.appendChild(head);
      rows.slice(0, 200).forEach(function (row) {
        var line = ce('div'); line.className = 'lc-row';
        line.style.gridTemplateColumns = 'repeat(' + cols.length + ',minmax(0,1fr))';
        line.innerHTML = cols.map(function (c) {
          var v = row && typeof row === 'object' ? row[c.key] : row;
          return '<span>' + esc(typeof v === 'object' ? JSON.stringify(v) : v) + '</span>';
        }).join('');
        list.appendChild(line);
      });
      box.appendChild(list);
    },
    'text-view': function (box, panel, data) {
      var pre = ce('pre'); pre.className = 'lc-code'; pre.textContent = textOf(data); box.appendChild(pre);
    },
    'log-list': function (box, panel, data) {
      var rows = rowsOf(data); var wrap = ce('div'); wrap.className = 'lc-log';
      if (!rows.length) { wrap.innerHTML = '<div>暂无日志</div>'; }
      rows.slice(0, 200).forEach(function (r) {
        var line = ce('div');
        line.textContent = typeof r === 'string' ? r : (r.msg || JSON.stringify(r));
        wrap.appendChild(line);
      });
      box.appendChild(wrap);
    },
    'code-editor': function (box, panel, data) {
      var ta = ce('textarea'); ta.className = 'lc-code'; ta.value = textOf(data); box.appendChild(ta);
      var save = (panel.actions || []).filter(function (a) { return a.method === 'POST'; })[0];
      if (!save) return;
      var bar = ce('div'); bar.className = 'lc-bar';
      var btn = ce('button'); btn.className = 'lc-btn primary'; btn.textContent = save.label;
      btn.onclick = function () {
        renderApi(save.method, save.path, !!save.write, { text: ta.value })
          .then(function () { notice(save.label + ' 成功'); })
          .catch(function (e) { notice(e.message, true); });
      };
      bar.appendChild(btn); box.appendChild(bar);
    },
    'form': function (box, panel, data) { WIDGETS['kv-grid'](box, panel, data); },
    'action-list': function (box, panel, data) { WIDGETS['kv-grid'](box, panel, data); }
  };

  var renderApi = null, notice = null;

  function bindActions(bar, panel, refresh) {
    (panel.actions || []).forEach(function (a) {
      var btn = ce('button');
      btn.className = 'lc-btn' + (a.write ? ' primary' : '');
      btn.textContent = a.label;
      btn.dataset.action = a.id;
      btn.onclick = function () {
        if (a.fields && a.fields.length) { openForm(bar.parentNode, a, submit); }
        else submit({});
      };
      bar.appendChild(btn);
      function submit(values) {
        var body = a.method === 'POST' ? values : undefined;
        renderApi(a.method, a.path, !!a.write, body)
          .then(function () { notice(a.label + ' 成功'); refresh(); })
          .catch(function (e) { notice(e.message, true); });
      }
    });
  }

  function openForm(host, action, submit) {
    var old = host.querySelector('.lc-form');
    if (old) old.parentNode.removeChild(old);
    var form = ce('div'); form.className = 'lc-form';
    action.fields.forEach(function (f) {
      var wide = f.type === 'textarea';
      var label = ce('label'); label.textContent = f.label + (f.required ? ' *' : '');
      var wrap = ce('div'); wrap.className = wide ? 'lc-wide' : '';
      var input = ce(wide ? 'textarea' : 'input');
      input.dataset.key = f.key;
      if (!wide) input.type = f.type === 'number' ? 'number' : 'text';
      wrap.appendChild(input);
      if (wide) { form.appendChild(wrap); }
      else { form.appendChild(label); form.appendChild(wrap); }
    });
    var bar = ce('div'); bar.className = 'lc-bar lc-wide';
    var ok = ce('button'); ok.className = 'lc-btn primary'; ok.textContent = '提交';
    var cancel = ce('button'); cancel.className = 'lc-btn'; cancel.textContent = '取消';
    cancel.onclick = function () { form.parentNode.removeChild(form); };
    ok.onclick = function () {
      var values = {}, missing = [];
      form.querySelectorAll('[data-key]').forEach(function (el) {
        values[el.dataset.key] = el.value;
        var spec = action.fields.filter(function (f) { return f.key === el.dataset.key; })[0];
        if (spec && spec.required && !el.value.trim()) missing.push(spec.label);
      });
      if (missing.length) { notice('缺少必填项：' + missing.join('、'), true); return; }
      submit(values);
      if (form.parentNode) form.parentNode.removeChild(form);
    };
    bar.appendChild(ok); bar.appendChild(cancel); form.appendChild(bar);
    host.appendChild(form);
  }

  function buildPanel(host, panel, refresh) {
    var wrap = ce('div'); wrap.className = 'lc-panel';
    var head = ce('div'); head.className = 'lc-title';
    head.innerHTML = svg('cube') + '<span>' + esc(panel.title || panel.id) + '</span>' +
      '<span class="lc-hint">' + esc(panel.endpoint.method + ' ' + panel.endpoint.path) + '</span>';
    wrap.appendChild(head);
    var body = ce('div'); body.className = 'lc-body'; wrap.appendChild(body);
    var widget = WIDGETS[panel.widget] || WIDGETS['kv-grid'];
    var load = function () {
      body.innerHTML = '<div class="lc-empty">加载中…</div>';
      renderApi(panel.endpoint.method, panel.endpoint.path, !!panel.endpoint.write, undefined)
        .then(function (data) { body.innerHTML = ''; widget(body, panel, data); })
        .catch(function (e) { body.innerHTML = '<div class="lc-empty">服务未启动或请求失败：' + esc(e.message) + '</div>'; });
    };
    if (panel.actions && panel.actions.length) {
      var bar = ce('div'); bar.className = 'lc-bar';
      bindActions(bar, panel, load); wrap.appendChild(bar);
    }
    host.appendChild(wrap);
    return load;
  }

  function render(caps, opts) {
    ensureStyle();
    opts = opts || {};
    var baseFor = opts.baseFor || function (p) { return p; };
    renderApi = makeApi(baseFor, opts.getPassword, opts.extraHeaders);
    notice = opts.notice || function () {};
    var coreStatic = !!opts.coreStatic;
    var container = opts.container || document.getElementById('moduleRoot') || document.body;
    var nav = opts.navEl || document.getElementById('nav');
    if (!caps || !caps.modules || !caps.modules.length) {
      container.innerHTML = '<div class="lc-empty">能力清单不可用：后端未声明任何模块，因此没有可展示的页面元素。</div>';
      return { modules: [], select: function () {} };
    }
    var modules = caps.modules.filter(function (m) {
      if (m.requires_lib && (caps.libs || []).indexOf(m.requires_lib) < 0) return false;
      return m.panels && m.panels.length;
    });

    // Static core sections that are NOT in the manifest must leave the DOM.
    if (coreStatic) {
      var allowed = {};
      modules.forEach(function (m) { if (CORE_PAGE[m.id]) allowed[CORE_PAGE[m.id]] = true; });
      document.querySelectorAll('section[data-page]').forEach(function (s) {
        if (s.getAttribute('data-page') !== 'feature' && !allowed[s.getAttribute('data-page')]) {
          s.parentNode.removeChild(s);
        }
      });
    }

    var dynamic = ce('div'); dynamic.id = 'lc-dynamic'; container.appendChild(dynamic);
    var sections = {}, loaders = {};
    modules.forEach(function (m) {
      if (coreStatic && CORE_PAGE[m.id]) return; // reuse the mature six-area DOM
      var sec = ce('section'); sec.setAttribute('data-mod', m.id); sec.hidden = true;
      var loadersForModule = m.panels.map(function (p) { return buildPanel(sec, p, function () { loaders[m.id].forEach(function (fn) { fn(); }); }); });
      loaders[m.id] = loadersForModule;
      sections[m.id] = sec; dynamic.appendChild(sec);
    });

    if (nav) {
      nav.innerHTML = '';
      var lastArea = null;
      modules.forEach(function (m) {
        if (m.area !== lastArea) {
          lastArea = m.area;
          var label = ce('div'); label.className = 'ls-nav-label lc-nav-label';
          label.textContent = m.area === 'feature' ? 'Component' : 'Workspace';
          nav.appendChild(label);
        }
        var btn = ce('button');
        btn.className = 'ls-nav-item'; btn.dataset.module = m.id;
        btn.innerHTML = svg(m.icon) + '<span>' + esc(m.title) + '</span>';
        btn.onclick = function () { controller.select(m.id); };
        nav.appendChild(btn);
      });
    }

    function select(id) {
      var m = modules.filter(function (x) { return x.id === id; })[0];
      if (!m) return;
      controller.current = id;
      if (opts.titleEl) opts.titleEl.textContent = m.title;
      if (opts.subEl) opts.subEl.textContent = (caps.component || '') + ' · ' + m.id;
      document.querySelectorAll('section[data-page]').forEach(function (s) { s.hidden = true; });
      Object.keys(sections).forEach(function (k) { sections[k].hidden = true; });
      if (nav) nav.querySelectorAll('.ls-nav-item').forEach(function (b) {
        b.className = 'ls-nav-item' + (b.dataset.module === id ? ' on' : '');
      });
      if (coreStatic && CORE_PAGE[id]) {
        if (window.__ls) window.__ls.go(CORE_PAGE[id]);
      } else if (sections[id]) {
        sections[id].hidden = false;
        (loaders[id] || []).forEach(function (fn) { fn(); });
      }
    }
    var controller = {
      modules: modules, select: select, sections: sections, current: null,
      reload: function () { if (controller.current) select(controller.current); }
    };
    if (modules.length) select(modules[0].id);
    return controller;
  }

  window.LanConsole = { render: render, makeApi: makeApi, rowsOf: rowsOf, widgets: WIDGETS };
})();
