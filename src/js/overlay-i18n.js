(function () {
  'use strict';

  var CATALOGS = {
  "de": {
    "Listening": "Hört zu",
    "LISTENING": "HÖRT ZU",
    "PROCESSING": "WIRD VERARBEITET",
    "RELEASE TO STOP": "ZUM STOPPEN LOSLASSEN",
    "Transcribe": "Transkribieren",
    "Agent Mode": "Agentenmodus",
    "Discard recording": "Aufnahme verwerfen",
    "Inserted": "Eingefügt",
    "Executed": "Ausgeführt",
    "Copied": "Kopiert",
    "Done": "Fertig",
    "Retry": "Wiederholen",
    "Copy": "Kopieren",
    "No speech detected": "Keine Sprache erkannt",
    "Failed": "Fehlgeschlagen",
    "Transcription failed": "Transkription fehlgeschlagen",
    "Recording busy": "Aufnahme läuft bereits",
    "Copied - paste with Ctrl+V": "Kopiert – mit Strg+V einfügen",
    "Copy failed": "Kopieren fehlgeschlagen",
    "Inserted (clipboard replaced)": "Eingefügt (Zwischenablage ersetzt)",
    "Inserted (standard mode)": "Eingefügt (Standardmodus)",
    "Keys blocked": "Schlüssel gesperrt",
    "Keys blocked ({delivered} of {total})": "Schlüssel gesperrt ({delivered} von {total})",
    "Blocked by permissions": "Durch Berechtigungen blockiert",
    "Insert failed": "Das Einfügen ist fehlgeschlagen",
    "Retry - {text}": "Wiederholen – {text}"
  },
  };

  var SUPPORTED = ['en', 'de'];
  var current = 'en';
  var applied = false;

  function normalize(tag) {
    var lower = String(tag).trim().replace(/_/g, '-').toLowerCase();
    var base = lower.split('-')[0];
    return base === 'de' ? 'de' : 'en';
  }

  function resolve(preference) {
    if (!preference || preference === 'system') {
      var primary = (navigator.languages && navigator.languages.length)
        ? navigator.languages[0]
        : (navigator.language || 'en');
      return normalize(primary);
    }
    return normalize(preference);
  }

  function t(key, params) {
    var catalog = CATALOGS[current] || {};
    var value = Object.prototype.hasOwnProperty.call(catalog, key) ? catalog[key] : key;
    if (params) {
      value = value.replace(/\{(\w+)\}/g, function (match, name) {
        return Object.prototype.hasOwnProperty.call(params, name) ? String(params[name]) : match;
      });
    }
    return value;
  }

  function tr(text) {
    if (!text) return text;
    var catalog = CATALOGS[current] || {};
    return Object.prototype.hasOwnProperty.call(catalog, text) ? catalog[text] : text;
  }

  function apply() {
    document.documentElement.lang = current;
    var nodes = document.querySelectorAll('[data-i18n]');
    for (var i = 0; i < nodes.length; i++) {
      var node = nodes[i];
      var key = node.getAttribute('data-i18n');
      var attrs = node.getAttribute('data-i18n-attr');
      if (attrs) {
        attrs.split(',').forEach(function (attr) { node.setAttribute(attr.trim(), t(key)); });
      } else {
        node.textContent = t(key);
      }
    }
  }

  function init(preference) {
    var next = resolve(preference);
    var changed = next !== current || !applied;
    current = next;
    if (changed) {
      apply();
      applied = true;
    }
    return current;
  }

  window.OverlayI18n = {
    init: init,
    t: t,
    tr: tr,
    apply: apply,
    getLocale: function () { return current; },
    languages: SUPPORTED,
  };

  var stored = null;
  try { stored = window.localStorage.getItem('fluence_ui_language'); } catch (e) {}
  init(stored || 'system');
})();
