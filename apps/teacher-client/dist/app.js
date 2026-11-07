/**
 * Room Node 教师端 —— 静态前端逻辑（无打包器，Tauri WebView 直接加载）。
 *
 * 网络请求：优先调用 Rust 命令 `api_request`（绕过 WebView CORS，见 src-tauri/src/main.rs）；
 * 在普通浏览器里预览静态页时退化为 fetch（需要控制节点放行 CORS）。
 * 数据接口（教师角色只读权限）：
 *   POST /api/v1/auth/login            → access_token / refresh_token / user
 *   GET  /api/v1/auth/me               → 当前账号
 *   GET  /api/v1/admin/machines        → device:read
 *   GET  /api/v1/admin/schedules       → schedule:read
 */
;(function () {
  'use strict'

  var STORAGE_KEYS = {
    base: 'room-node.teacher.base',
    token: 'room-node.teacher.access-token',
    refresh: 'room-node.teacher.refresh-token',
    user: 'room-node.teacher.user',
    username: 'room-node.teacher.username',
  }

  var state = {
    baseUrl: '',
    accessToken: '',
    refreshToken: '',
    user: null,
    toastTimer: 0,
  }

  var els = {}
  var sessionExpiredGuard = false

  // ------------------------------------------------------------------ 工具

  function $(id) {
    return document.getElementById(id)
  }

  function cacheElements() {
    els.loginView = $('login-view')
    els.dashboardView = $('dashboard-view')
    els.loginForm = $('login-form')
    els.baseUrl = $('base-url')
    els.username = $('username')
    els.password = $('password')
    els.loginError = $('login-error')
    els.loginSubmit = $('login-submit')
    els.nodeLabel = $('node-label')
    els.userLabel = $('user-label')
    els.userRoles = $('user-roles')
    els.userAvatar = $('user-avatar')
    els.refreshBtn = $('refresh-btn')
    els.openWorkspaceBtn = $('open-workspace-btn')
    els.logoutBtn = $('logout-btn')
    els.banner = $('banner')
    els.dataUpdated = $('data-updated')
    els.statMachines = $('stat-machines')
    els.statOnline = $('stat-online')
    els.statSchedules = $('stat-schedules')
    els.statRole = $('stat-role')
    els.machinesBody = $('machines-body')
    els.machinesStatus = $('machines-status')
    els.machinesEmpty = $('machines-empty')
    els.schedulesBody = $('schedules-body')
    els.schedulesStatus = $('schedules-status')
    els.schedulesEmpty = $('schedules-empty')
    els.toast = $('toast')
  }

  /** 规范化控制节点地址：仅 http(s)，去掉结尾斜杠。 */
  function normalizeBaseUrl(raw) {
    var value = String(raw || '')
      .trim()
      .replace(/\/+$/, '')
    if (!value) return ''
    try {
      var url = new URL(value)
      if (url.protocol !== 'http:' && url.protocol !== 'https:') return ''
      // 允许用户填带路径的部署地址（例如 https://console.example.com/room），保留路径。
      return url.toString().replace(/\/+$/, '')
    } catch (error) {
      return ''
    }
  }

  function tauriInvoke() {
    return window.__TAURI__ && window.__TAURI__.core ? window.__TAURI__.core.invoke : null
  }

  function setBusy(button, busy) {
    if (!button) return
    button.disabled = busy
    button.classList.toggle('is-loading', busy)
  }

  function showToast(message) {
    if (!els.toast) return
    els.toast.textContent = message
    els.toast.hidden = false
    window.clearTimeout(state.toastTimer)
    state.toastTimer = window.setTimeout(function () {
      els.toast.hidden = true
    }, 2600)
  }

  function showLoginError(message) {
    if (!els.loginError) return
    els.loginError.textContent = message
    els.loginError.hidden = !message
  }

  function renderBanner(messages) {
    if (!els.banner) return
    if (!messages || !messages.length) {
      els.banner.hidden = true
      els.banner.textContent = ''
      return
    }
    els.banner.textContent = messages.join('\n')
    els.banner.hidden = false
  }

  function formatTime(date) {
    try {
      return date.toLocaleTimeString('zh-CN', { hour12: false })
    } catch (error) {
      return date.toISOString().slice(11, 19)
    }
  }

  function setSectionStatus(element, message) {
    if (element) element.textContent = message
  }

  function clearChildren(element) {
    if (!element) return
    while (element.firstChild) element.removeChild(element.firstChild)
  }

  // ------------------------------------------------------------------ 持久化

  function persistSession() {
    try {
      localStorage.setItem(STORAGE_KEYS.base, state.baseUrl)
      localStorage.setItem(STORAGE_KEYS.token, state.accessToken)
      localStorage.setItem(STORAGE_KEYS.refresh, state.refreshToken)
      localStorage.setItem(STORAGE_KEYS.user, JSON.stringify(state.user || null))
      if (state.user && state.user.username) {
        localStorage.setItem(STORAGE_KEYS.username, state.user.username)
      }
    } catch (error) {
      showToast('无法写入 localStorage：' + (error.message || error))
    }
  }

  function persistUser() {
    try {
      localStorage.setItem(STORAGE_KEYS.user, JSON.stringify(state.user || null))
    } catch (error) {
      /* 忽略：账号信息只是缓存 */
    }
  }

  function clearSessionStorage() {
    try {
      localStorage.removeItem(STORAGE_KEYS.token)
      localStorage.removeItem(STORAGE_KEYS.refresh)
      localStorage.removeItem(STORAGE_KEYS.user)
    } catch (error) {
      /* 忽略 */
    }
  }

  function readStoredSession() {
    try {
      var base = normalizeBaseUrl(localStorage.getItem(STORAGE_KEYS.base) || '')
      var token = localStorage.getItem(STORAGE_KEYS.token) || ''
      var refresh = localStorage.getItem(STORAGE_KEYS.refresh) || ''
      var user = null
      try {
        user = JSON.parse(localStorage.getItem(STORAGE_KEYS.user) || 'null')
      } catch (error) {
        user = null
      }
      var username = localStorage.getItem(STORAGE_KEYS.username) || ''
      return { base: base, token: token, refresh: refresh, user: user, username: username }
    } catch (error) {
      return { base: '', token: '', refresh: '', user: null, username: '' }
    }
  }

  // ------------------------------------------------------------------ HTTP

  /** 统一请求：Tauri 内走 Rust 命令（无 CORS），浏览器预览走 fetch。 */
  function request(method, path, options) {
    var config = options || {}
    var url = state.baseUrl + path
    var invoke = tauriInvoke()
    if (invoke) {
      return invoke('api_request', {
        method: method,
        url: url,
        token: config.token || null,
        body: config.body || null,
      }).catch(function (error) {
        throw new Error('无法连接控制节点：' + String(error))
      })
    }
    var headers = { Accept: 'application/json' }
    if (config.body) headers['Content-Type'] = 'application/json'
    if (config.token) headers.Authorization = 'Bearer ' + config.token
    return fetch(url, {
      method: method,
      headers: headers,
      body: config.body ? JSON.stringify(config.body) : undefined,
    }).then(function (response) {
      return response.text().then(function (text) {
        var parsed = null
        if (text) {
          try {
            parsed = JSON.parse(text)
          } catch (error) {
            parsed = text
          }
        }
        return { status: response.status, body: parsed }
      })
    })
  }

  function apiErrorMessage(result, fallback) {
    var body = result && result.body
    if (body && typeof body === 'object') {
      if (body.error && typeof body.error.message === 'string' && body.error.message) {
        return body.error.message
      }
      if (typeof body.message === 'string' && body.message) return body.message
    }
    if (typeof body === 'string' && body.trim()) return body.trim().slice(0, 200)
    if (result && result.status) return fallback + '（HTTP ' + result.status + '）'
    return fallback
  }

  function SessionExpiredError(message) {
    this.name = 'SessionExpiredError'
    this.message = message || '登录已过期，请重新登录'
    this.stack = new Error(this.message).stack
  }
  SessionExpiredError.prototype = Object.create(Error.prototype)
  SessionExpiredError.prototype.constructor = SessionExpiredError

  function authorizedGet(path) {
    return request('GET', path, { token: state.accessToken }).then(function (result) {
      if (result.status === 401) throw new SessionExpiredError()
      return result
    })
  }

  // ------------------------------------------------------------------ 视图切换

  function showLoginView(message) {
    if (els.dashboardView) els.dashboardView.hidden = true
    if (els.loginView) els.loginView.hidden = false
    if (els.password) els.password.value = ''
    showLoginError(message || '')
    if (els.username) els.username.focus()
  }

  function showDashboardView() {
    if (els.loginView) els.loginView.hidden = true
    if (els.dashboardView) els.dashboardView.hidden = false
  }

  function handleUnauthorized(message) {
    if (sessionExpiredGuard) return
    sessionExpiredGuard = true
    state.accessToken = ''
    state.refreshToken = ''
    state.user = null
    clearSessionStorage()
    showLoginView(message || '登录已过期，请重新登录')
    renderBanner([])
    window.setTimeout(function () {
      sessionExpiredGuard = false
    }, 1000)
  }

  // ------------------------------------------------------------------ 登录 / 退出

  function login(event) {
    if (event) event.preventDefault()
    var base = normalizeBaseUrl(els.baseUrl.value)
    if (!base) {
      showLoginError('控制节点地址需为 http(s)://host[:port]')
      return
    }
    var username = String(els.username.value || '').trim()
    var password = String(els.password.value || '')
    if (!username) {
      showLoginError('请填写账号')
      return
    }
    if (!password) {
      showLoginError('请填写密码')
      return
    }

    state.baseUrl = base
    showLoginError('')
    setBusy(els.loginSubmit, true)
    request('POST', '/api/v1/auth/login', {
      body: { username: username, password: password },
    })
      .then(function (result) {
        if (result.status === 401 || result.status === 403) {
          throw new Error('账号或密码错误，或账号已被锁定')
        }
        if (result.status === 429) {
          throw new Error('登录尝试过于频繁，请稍后再试')
        }
        if (result.status !== 200) {
          throw new Error(apiErrorMessage(result, '登录失败'))
        }
        var data = result.body || {}
        if (!data.access_token) throw new Error('登录响应缺少 access_token')
        state.accessToken = data.access_token
        state.refreshToken = typeof data.refresh_token === 'string' ? data.refresh_token : ''
        state.user = data.user || null
        persistSession()
        if (els.baseUrl) els.baseUrl.value = state.baseUrl
        if (els.password) els.password.value = ''
        showDashboardView()
        renderUser()
        return refreshAll()
      })
      .catch(function (error) {
        showLoginError(error && error.message ? error.message : '登录失败')
      })
      .finally(function () {
        setBusy(els.loginSubmit, false)
      })
  }

  function logout() {
    var token = state.accessToken
    var refreshToken = state.refreshToken
    state.accessToken = ''
    state.refreshToken = ''
    state.user = null
    clearSessionStorage()
    showLoginView('')
    renderBanner([])
    setSectionStatus(els.machinesStatus, '')
    setSectionStatus(els.schedulesStatus, '')
    if (!token || !refreshToken) return
    request('POST', '/api/v1/auth/logout', {
      token: token,
      body: { refresh_token: refreshToken },
    }).catch(function () {
      /* 退出接口失败不影响本地清理 */
    })
  }

  // ------------------------------------------------------------------ 数据加载

  function loadMe() {
    return authorizedGet('/api/v1/auth/me')
      .then(function (result) {
        if (result.status !== 200) {
          throw new Error(apiErrorMessage(result, '读取账号信息失败'))
        }
        state.user = result.body
        persistUser()
        renderUser()
      })
      .catch(function (error) {
        if (error instanceof SessionExpiredError) throw error
        throw new Error(error && error.message ? error.message : '读取账号信息失败')
      })
  }

  function loadMachines() {
    setSectionStatus(els.machinesStatus, '加载中…')
    return authorizedGet('/api/v1/admin/machines?page=1&page_size=100')
      .then(function (result) {
        if (result.status === 403) {
          setSectionStatus(els.machinesStatus, '缺少 device:read 权限')
          renderMachines([])
          return { total: 0 }
        }
        if (result.status !== 200) {
          throw new Error(apiErrorMessage(result, '读取机器列表失败'))
        }
        var page = result.body || {}
        var items = Array.isArray(page.items) ? page.items : []
        renderMachines(items)
        setSectionStatus(els.machinesStatus, '共 ' + (page.total != null ? page.total : items.length) + ' 台')
        return { total: items.length }
      })
      .catch(function (error) {
        if (error instanceof SessionExpiredError) throw error
        setSectionStatus(els.machinesStatus, '加载失败')
        throw new Error('机器列表：' + (error && error.message ? error.message : '未知错误'))
      })
  }

  function loadSchedules() {
    setSectionStatus(els.schedulesStatus, '加载中…')
    return authorizedGet('/api/v1/admin/schedules?page=1&page_size=100')
      .then(function (result) {
        if (result.status === 403) {
          setSectionStatus(els.schedulesStatus, '缺少 schedule:read 权限')
          renderSchedules([])
          return { total: 0 }
        }
        if (result.status !== 200) {
          throw new Error(apiErrorMessage(result, '读取课程列表失败'))
        }
        var page = result.body || {}
        var items = Array.isArray(page.items) ? page.items : []
        renderSchedules(items)
        setSectionStatus(
          els.schedulesStatus,
          '共 ' + (page.total != null ? page.total : items.length) + ' 条',
        )
        return { total: items.length }
      })
      .catch(function (error) {
        if (error instanceof SessionExpiredError) throw error
        setSectionStatus(els.schedulesStatus, '加载失败')
        throw new Error('课程列表：' + (error && error.message ? error.message : '未知错误'))
      })
  }

  function refreshAll() {
    if (!state.accessToken) return Promise.resolve()
    setBusy(els.refreshBtn, true)
    renderBanner([])
    return Promise.allSettled([loadMe(), loadMachines(), loadSchedules()])
      .then(function (results) {
        var expired = results.some(function (item) {
          return item.status === 'rejected' && item.reason instanceof SessionExpiredError
        })
        if (expired) {
          handleUnauthorized('登录已过期，请重新登录')
          return
        }
        var messages = results
          .filter(function (item) {
            return item.status === 'rejected'
          })
          .map(function (item) {
            return item.reason && item.reason.message ? item.reason.message : '数据加载失败'
          })
        renderBanner(messages)
        updateStats()
        if (els.dataUpdated) {
          els.dataUpdated.textContent = '更新于 ' + formatTime(new Date())
        }
      })
      .finally(function () {
        setBusy(els.refreshBtn, false)
      })
  }

  // ------------------------------------------------------------------ 渲染

  function badge(label, tone) {
    var span = document.createElement('span')
    span.className = 'badge badge-' + tone
    span.textContent = label
    return span
  }

  function onlineMeta(online) {
    if (online === 'online') return badge('在线', 'ok')
    if (online === 'offline') return badge('离线', 'bad')
    return badge('无会话', 'muted')
  }

  function healthMeta(report) {
    var status = report && report.health_status
    if (status === 'ok') return badge('正常', 'ok')
    if (status === 'warning') return badge('告警', 'warn')
    if (status === 'critical') return badge('严重', 'bad')
    if (status === 'missing') return badge('未上报', 'muted')
    return badge('未知', 'muted')
  }

  function textCell(value, className) {
    var cell = document.createElement('td')
    cell.textContent = value == null || value === '' ? '—' : String(value)
    if (className) cell.className = className
    return cell
  }

  function badgeCell(node) {
    var cell = document.createElement('td')
    cell.appendChild(node)
    return cell
  }

  function renderUser() {
    var user = state.user || {}
    var displayName = user.display_name || user.username || '未知用户'
    if (els.userLabel) els.userLabel.textContent = displayName
    if (els.userAvatar) {
      els.userAvatar.textContent = String(displayName).slice(0, 1).toUpperCase()
    }
    var roles = Array.isArray(user.roles) ? user.roles : []
    if (els.userRoles) {
      els.userRoles.textContent = roles.length ? '角色：' + roles.join(' / ') : '角色：—'
    }
    if (els.statRole) {
      els.statRole.textContent = roles.length ? roles[0] : '—'
    }
    if (els.username && !els.username.value) els.username.value = user.username || ''
    if (els.nodeLabel) els.nodeLabel.textContent = state.baseUrl || '未连接'
  }

  function renderMachines(items) {
    var body = els.machinesBody
    if (!body) return
    clearChildren(body)
    items.forEach(function (item) {
      var row = document.createElement('tr')
      row.appendChild(textCell(item.device_slot || item.serial, 'truncate'))
      row.appendChild(textCell(item.serial, 'mono truncate'))
      row.appendChild(textCell(item.room_name || '未绑定'))
      row.appendChild(badgeCell(onlineMeta(item.online)))
      row.appendChild(badgeCell(healthMeta(item.report)))
      body.appendChild(row)
    })
    if (els.machinesEmpty) els.machinesEmpty.hidden = items.length > 0
    if (els.statMachines) els.statMachines.textContent = String(items.length)
    if (els.statOnline) {
      var online = items.filter(function (item) {
        return item.online === 'online'
      }).length
      els.statOnline.textContent = String(online)
    }
  }

  function renderSchedules(items) {
    var body = els.schedulesBody
    if (!body) return
    clearChildren(body)
    items.forEach(function (item) {
      var row = document.createElement('tr')
      row.appendChild(textCell(item.name || item.id))
      row.appendChild(textCell(item.room_name || '—'))
      row.appendChild(textCell((item.start_date || '—') + ' ~ ' + (item.end_date || '—')))
      row.appendChild(badgeCell(item.is_active ? badge('启用', 'ok') : badge('停用', 'muted')))
      body.appendChild(row)
    })
    if (els.schedulesEmpty) els.schedulesEmpty.hidden = items.length > 0
    if (els.statSchedules) els.statSchedules.textContent = String(items.length)
  }

  function updateStats() {
    if (els.statMachines && els.machinesBody) {
      els.statMachines.textContent = String(els.machinesBody.children.length)
    }
    if (els.statSchedules && els.schedulesBody) {
      els.statSchedules.textContent = String(els.schedulesBody.children.length)
    }
  }

  // ------------------------------------------------------------------ 打开 Web 工作台

  function openWorkspace() {
    if (!state.baseUrl) {
      showToast('尚未登录，无法打开教师工作台')
      return
    }
    var url = state.baseUrl + '/teacher'
    var invoke = tauriInvoke()
    if (invoke) {
      invoke('open_external', { url: url })
        .then(function () {
          showToast('已在系统浏览器中打开教师工作台')
        })
        .catch(function (error) {
          renderBanner(['打开系统浏览器失败：' + String(error)])
        })
      return
    }
    // 浏览器预览降级：新标签页打开。
    window.open(url, '_blank', 'noopener')
    showToast('已在新标签页打开教师工作台')
  }

  // ------------------------------------------------------------------ 启动

  function bindEvents() {
    els.loginForm.addEventListener('submit', login)
    els.refreshBtn.addEventListener('click', function () {
      refreshAll()
    })
    els.logoutBtn.addEventListener('click', logout)
    els.openWorkspaceBtn.addEventListener('click', openWorkspace)
  }

  function restoreSession() {
    var stored = readStoredSession()
    if (stored.base) els.baseUrl.value = stored.base
    if (stored.username) els.username.value = stored.username
    if (!stored.token || !stored.base) return false
    state.baseUrl = stored.base
    state.accessToken = stored.token
    state.refreshToken = stored.refresh
    state.user = stored.user
    showDashboardView()
    renderUser()
    refreshAll()
    return true
  }

  function bootstrap() {
    cacheElements()
    bindEvents()
    // 输入框默认值由 index.html 提供；普通浏览器预览时自动走 fetch 降级。
    if (!restoreSession()) {
      showLoginView('')
    }
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', bootstrap)
  } else {
    bootstrap()
  }
})()
