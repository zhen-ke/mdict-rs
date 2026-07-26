// =============================================
// MDict 极速词典 - 增强版
// =============================================

// 配置
const CONFIG = {
    HISTORY_MAX: 20,           // 最大历史记录数
    DEBOUNCE_MS: 200,          // 搜索防抖延迟
    HISTORY_KEY: 'mdx_history', // localStorage 键名
    COLLAPSE_KEY: 'mdx_collapsed_dicts', // localStorage: 记住用户折叠的词典
    DICT_FILTER_KEY: 'mdx_dict_filter'   // localStorage: 记住用户启用的词典
};

// 全局状态
let suggestTimer = null;
let selectedIndex = -1;
let currentQuery = '';  // 当前查询词，用于高亮
let isNavigating = false; // 防止重复导航
let allDicts = [];         // [{id, name, ...}] 从 /api/dicts 加载

// =============================================
// 词典折叠/展开 + 快速导航
// =============================================

/**
 * 获取用户折叠偏好（dict_id Set）
 */
function getCollapsedDicts() {
    try {
        const raw = localStorage.getItem(CONFIG.COLLAPSE_KEY);
        return raw ? new Set(JSON.parse(raw)) : new Set();
    } catch (_) {
        return new Set();
    }
}

/**
 * 保存用户折叠偏好
 */
function saveCollapsedDicts(collapsedSet) {
    try {
        localStorage.setItem(CONFIG.COLLAPSE_KEY, JSON.stringify([...collapsedSet]));
    } catch (_) {}
}

/**
 * 切换单个词典 section 的折叠状态
 */
function toggleSection($section) {
    const dictId = $section.data('dict-id');
    const collapsed = getCollapsedDicts();

    $section.toggleClass('collapsed');

    if ($section.hasClass('collapsed')) {
        collapsed.add(dictId);
    } else {
        collapsed.delete(dictId);
    }
    saveCollapsedDicts(collapsed);
}

/**
 * 对聚合查询结果做后处理：
 *   1. 给每个 section header 添加折叠箭头
 *   2. 恢复用户的折叠偏好（首次访问默认只展开第 1 个）
 *   3. 在 meta 行右侧添加词典快速跳转 pill
 *   4. 绑定点击事件
 */
function enhanceAggregateResult() {
    const $aggregate = $('#mdx-resp .mdict-aggregate');
    if ($aggregate.length === 0) {
        teardownDictNavObserver();
        return;
    }

    const $sections = $aggregate.find('.mdict-dict-section');
    if ($sections.length <= 1) {
        teardownDictNavObserver(); // 单词典不需要导航，拆掉上一轮的 observer
        return;
    }

    const collapsed = getCollapsedDicts();
    const isFirstVisit = collapsed.size === 0
        && !localStorage.getItem(CONFIG.COLLAPSE_KEY);

    // 1. 添加折叠箭头 + 恢复折叠状态
    $sections.each(function (idx) {
        const $sec = $(this);
        const $head = $sec.find('.mdict-dict-head');
        const dictId = $sec.data('dict-id');

        // 追加折叠箭头（如果还没有）
        if ($head.find('.mdict-dict-toggle').length === 0) {
            $head.append('<span class="mdict-dict-toggle">▼</span>');
        }

        // 首次访问：只展开第 1 个；后续按用户偏好
        if (isFirstVisit) {
            if (idx > 0) $sec.addClass('collapsed');
        } else {
            if (collapsed.has(dictId)) $sec.addClass('collapsed');
        }
    });

    // 首次访问时，把默认折叠状态写入 localStorage
    if (isFirstVisit) {
        const initCollapsed = new Set();
        $sections.each(function (idx) {
            if (idx > 0) initCollapsed.add($(this).data('dict-id'));
        });
        saveCollapsedDicts(initCollapsed);
    }

    // 2. 在 meta 行添加快速跳转 pill + 全部展开/折叠
    const $meta = $aggregate.find('.mdict-aggregate-meta');
    if ($meta.find('.mdict-dict-nav').length === 0) {
        const $nav = $('<span class="mdict-dict-nav"></span>');
        $sections.each(function (idx) {
            const $sec = $(this);
            const dictId = $sec.data('dict-id');
            const name = $sec.find('.mdict-dict-name').text() || dictId;
            const label = name.length > 12 ? name.slice(0, 12) + '…' : name;
            // 用 DOM API 构建，避免 HTML 注入
            $('<button class="mdict-dict-nav-pill"></button>')
                .attr('data-target', dictId)
                .attr('title', name)
                .text((idx + 1) + '. ' + label)
                .appendTo($nav);
        });
        $meta.append($nav);
        $meta.append('<button class="mdict-toggle-all" data-action="expand">全部展开</button>');
    }

    // 3. 启动滚动观察，高亮当前可见词典对应的 nav pill
    setupDictNavObserver();
}

/**
 * 滚动时高亮当前可见词典对应的导航 pill。
 * 每次查询都会重建 #mdx-resp 内容，因此先拆掉上一次的 observer。
 */
let dictNavObserver = null;
function teardownDictNavObserver() {
    if (dictNavObserver) {
        dictNavObserver.disconnect();
        dictNavObserver = null;
    }
}
function setupDictNavObserver() {
    teardownDictNavObserver();
    const sections = document.querySelectorAll('#mdx-resp .mdict-dict-section');
    if (sections.length === 0) return;

    const pills = {};
    document.querySelectorAll('#mdx-resp .mdict-dict-nav-pill').forEach(function (pill) {
        pills[pill.getAttribute('data-target')] = pill;
    });

    const visible = new Map();
    dictNavObserver = new IntersectionObserver(function (entries) {
        entries.forEach(function (entry) {
            const id = entry.target.getAttribute('data-dict-id');
            if (entry.isIntersecting) {
                visible.set(id, entry.intersectionRatio);
            } else {
                visible.delete(id);
            }
        });
        let bestId = null;
        let bestRatio = 0;
        visible.forEach(function (ratio, id) {
            if (ratio > bestRatio) {
                bestRatio = ratio;
                bestId = id;
            }
        });
        document.querySelectorAll('#mdx-resp .mdict-dict-nav-pill.active').forEach(function (p) {
            p.classList.remove('active');
        });
        if (bestId && pills[bestId]) {
            pills[bestId].classList.add('active');
        }
    }, {
        // 当 section 顶端进入视口上 20%、且底部还在视口下 40% 区间时视为“焦点”
        rootMargin: '-20% 0px -60% 0px',
        threshold: [0, 0.1, 0.25, 0.5, 0.75, 1]
    });

    sections.forEach(function (sec) {
        dictNavObserver.observe(sec);
    });
}

/**
 * 事件委托：点击 header 折叠/展开
 */
$(document).on('click', '#mdx-resp .mdict-dict-head', function (e) {
    // 不拦截 header 内链接的点击
    if ($(e.target).closest('a').length) return;
    const $section = $(this).closest('.mdict-dict-section');
    toggleSection($section);
});

/**
 * 事件委托：点击导航 pill 跳转到对应词典
 */
$(document).on('click', '.mdict-dict-nav-pill', function () {
    const targetId = $(this).data('target');
    const $target = $('#mdx-resp .mdict-dict-section[data-dict-id="' + targetId + '"]');
    if ($target.length === 0) return;

    // 如果目标是折叠的，先展开
    if ($target.hasClass('collapsed')) {
        toggleSection($target);
    }

    // 滚动到目标
    $target[0].scrollIntoView({ behavior: 'smooth', block: 'start' });
});

/**
 * 事件委托：全部展开/折叠
 */
$(document).on('click', '.mdict-toggle-all', function () {
    const $btn = $(this);
    const action = $btn.data('action');
    const $sections = $('#mdx-resp .mdict-dict-section');
    const collapsed = getCollapsedDicts();

    if (action === 'expand') {
        $sections.removeClass('collapsed');
        collapsed.clear();
        $btn.text('全部折叠').data('action', 'collapse');
    } else {
        $sections.addClass('collapsed');
        $sections.each(function () {
            collapsed.add($(this).data('dict-id'));
        });
        $btn.text('全部展开').data('action', 'expand');
    }
    saveCollapsedDicts(collapsed);
});

// =============================================
// 词典筛选功能
// =============================================

/**
 * 获取用户启用的词典 ID 集合。
 * 返回 null 表示"全部启用"（默认状态 / 从未筛选过）。
 */
function getEnabledDictIds() {
    try {
        const raw = localStorage.getItem(CONFIG.DICT_FILTER_KEY);
        if (!raw) return null; // 从未设置 → 全部
        const ids = JSON.parse(raw);
        if (!Array.isArray(ids) || ids.length === 0) return null;
        return new Set(ids);
    } catch (_) {
        return null;
    }
}

/**
 * 保存启用的词典 ID 集合。
 * 传入 null 表示恢复到"全部启用"。
 */
function saveEnabledDictIds(idSet) {
    try {
        if (!idSet) {
            localStorage.removeItem(CONFIG.DICT_FILTER_KEY);
        } else {
            localStorage.setItem(CONFIG.DICT_FILTER_KEY, JSON.stringify([...idSet]));
        }
    } catch (_) {}
}

/**
 * 构建 dicts 查询参数（逗号分隔的 ID 串）。
 * 如果全选或未筛选，返回空字符串（后端视为查所有）。
 */
function buildDictsParam() {
    const enabled = getEnabledDictIds();
    if (!enabled || allDicts.length === 0) return '';
    // 全部勾选 → 等同于不筛选
    if (enabled.size >= allDicts.length) return '';
    return [...enabled].join(',');
}

/**
 * 加载词典列表并渲染筛选面板。
 * 只在有 2+ 本词典时才显示面板。
 */
function initDictFilter() {
    $.getJSON('./api/dicts', function (dicts) {
        allDicts = dicts || [];
        if (allDicts.length < 2) return; // 1 本词典不需要筛选

        const enabled = getEnabledDictIds(); // null = 全部启用
        const $panel = $('<div class="dict-filter-panel"></div>');
        const $toggle = $('<button class="dict-filter-toggle" title="词典筛选"><svg class="icon"><use href="#icon-filter"/></svg> 筛选</button>');
        const $body = $('<div class="dict-filter-body" style="display:none"></div>');

        // "全选 / 全不选" 控制行
        const $ctrl = $('<div class="dict-filter-ctrl"></div>');
        $ctrl.append('<button class="dict-filter-ctrl-btn" data-action="all">全选</button>');
        $ctrl.append('<button class="dict-filter-ctrl-btn" data-action="none">全不选</button>');
        $body.append($ctrl);

        allDicts.forEach(function (d) {
            const checked = !enabled || enabled.has(d.id);
            const $label = $('<label class="dict-filter-item"></label>');
            const $cb = $('<input type="checkbox">')
                .attr('data-dict-id', d.id)
                .prop('checked', checked);
            const displayName = d.name.length > 20 ? d.name.slice(0, 20) + '…' : d.name;
            $label.append($cb).append(' ' + displayName);
            $body.append($label);
        });

        $panel.append($toggle).append($body);
        $('.search-container').append($panel);

        // 切换面板显示
        $toggle.on('click', function () {
            $body.slideToggle(150);
        });

        // 点击页面其他区域关闭面板
        $(document).on('click', function (e) {
            if (!$(e.target).closest('.dict-filter-panel').length) {
                $body.slideUp(150);
            }
        });

        // 单个 checkbox 变化
        $body.on('change', 'input[type="checkbox"]', function () {
            syncFilterFromCheckboxes($body);
        });

        // 全选 / 全不选
        $body.on('click', '.dict-filter-ctrl-btn', function () {
            const action = $(this).data('action');
            const newState = (action === 'all');
            $body.find('input[type="checkbox"]').prop('checked', newState);
            syncFilterFromCheckboxes($body);
        });
    });
}

/**
 * 从 checkbox 状态同步到 localStorage。
 */
function syncFilterFromCheckboxes($body) {
    const checked = [];
    $body.find('input[type="checkbox"]').each(function () {
        if ($(this).prop('checked')) {
            checked.push($(this).data('dict-id'));
        }
    });
    if (checked.length === 0 || checked.length === allDicts.length) {
        // 全选或全不选 → 存为 null（查所有）
        saveEnabledDictIds(null);
    } else {
        saveEnabledDictIds(new Set(checked));
    }
}

// =============================================
// URL 路由功能
// =============================================

/**
 * 从 URL hash 获取查询词
 * 支持格式: #/word/dictionary 或 #dictionary
 */
function getWordFromUrl() {
    const hash = window.location.hash;
    if (!hash) return null;

    // 格式: #/word/dictionary
    const match = hash.match(/^#\/word\/(.+)$/);
    if (match) {
        return decodeURIComponent(match[1]);
    }

    // 简单格式: #dictionary
    if (hash.length > 1 && !hash.startsWith('#/')) {
        return decodeURIComponent(hash.slice(1));
    }

    return null;
}

/**
 * 更新 URL hash（不触发 hashchange 事件的查询）
 */
function updateUrl(word, addToHistory = true) {
    if (!word) {
        if (window.location.hash) {
            history.pushState(null, '', window.location.pathname);
        }
        return;
    }

    const newHash = '#/word/' + encodeURIComponent(word);

    if (addToHistory) {
        // 添加到浏览器历史
        history.pushState({ word: word }, '', newHash);
    } else {
        // 替换当前状态（不添加历史记录）
        history.replaceState({ word: word }, '', newHash);
    }

    // 更新页面标题
    document.title = word + ' - MDict 极速词典';
}

/**
 * 处理浏览器后退/前进
 */
function handlePopState(event) {
    const word = getWordFromUrl();

    if (word) {
        isNavigating = true;
        $('#word').val(word);
        queryMdxFromNavigation(word);
    } else {
        // 回到首页状态
        $('#word').val('');
        showWelcome();
        document.title = 'MDict 极速词典';
    }
}

/**
 * 从导航触发的查询（不更新 URL）
 */
function queryMdxFromNavigation(word) {
    if (!word || !validInput(word)) {
        return;
    }

    currentQuery = word;
    showLoading();
    hideHistoryDropdown();
    hideSuggestions();

    const navData = { 'word': word };
    const dictsP = buildDictsParam();
    if (dictsP) navData.dicts = dictsP;

    $.ajax({
        url: './query',
        type: 'POST',
        data: navData,
        dataType: 'html',
        success: function (data) {
            isNavigating = false;
            if (data && data.trim() !== '' && !data.includes('not found')) {
                let highlighted = highlightSearchTerm(data, currentQuery);
                $('#mdx-resp').html(highlighted).show();
                enhanceAggregateResult();
                $('#share-btn').show();
                document.title = word + ' - MDict 极速词典';
            } else {
                showEmpty();
            }
        },
        error: function(xhr) {
            isNavigating = false;
            if (xhr.status === 404) {
                showEmpty();
            } else {
                showError('查询出错，请稍后重试');
            }
        }
    });
}

/**
 * 显示欢迎页面
 */
function showWelcome() {
    $('#mdx-resp').html(`
        <div class="empty-state">
            <div class="empty-state-icon"><svg class="icon"><use href="#icon-search"/></svg></div>
            <p>请在上方输入单词开始查询</p>
        </div>
    `);
}

// 监听浏览器后退/前进
window.addEventListener('popstate', handlePopState);

// =============================================
// 初始化
// =============================================
$(document).ready(function () {
    initHistoryDropdown();
    initDictFilter();

    // 调试开关：URL 含 ?debug 时露出词典 ID / 调试信息
    if (location.search.indexOf('debug') !== -1) {
        $('#mdx-resp').addClass('show-dict-id');
    }

    // 检查 URL 是否有查询词
    const wordFromUrl = getWordFromUrl();
    if (wordFromUrl) {
        $('#word').val(wordFromUrl);
        queryMdxFromNavigation(wordFromUrl);
    } else {
        $('#word').focus();
    }
});

// =============================================
// 查询历史功能 (localStorage)
// =============================================
function getHistory() {
    try {
        return JSON.parse(localStorage.getItem(CONFIG.HISTORY_KEY)) || [];
    } catch (e) {
        return [];
    }
}

function saveHistory(word) {
    if (!word || word.length < 2) return;

    let history = getHistory();
    // 移除重复项
    history = history.filter(w => w.toLowerCase() !== word.toLowerCase());
    // 添加到开头
    history.unshift(word);
    // 限制数量
    if (history.length > CONFIG.HISTORY_MAX) {
        history = history.slice(0, CONFIG.HISTORY_MAX);
    }
    localStorage.setItem(CONFIG.HISTORY_KEY, JSON.stringify(history));
}

function clearHistory() {
    localStorage.removeItem(CONFIG.HISTORY_KEY);
    hideHistoryDropdown();
}

function initHistoryDropdown() {
    // 创建历史记录下拉框
    if ($('#history-dropdown').length === 0) {
        $('#search-wrapper').append(`
            <div id="history-dropdown" class="history-dropdown" style="display: none;">
                <div class="history-header">
                    <span><svg class="icon"><use href="#icon-clock"/></svg> 查询历史</span>
                    <button id="clear-history" class="clear-history-btn">清空</button>
                </div>
                <ul id="history-list"></ul>
            </div>
        `);
    }
}

function showHistoryDropdown() {
    const history = getHistory();
    if (history.length === 0) return;

    const $list = $('#history-list');
    $list.empty();

    history.forEach((word, index) => {
        $list.append(`<li data-word="${word}">${word}</li>`);
    });

    $('#history-dropdown').show();
    $('#suggestions').hide();
}

function hideHistoryDropdown() {
    $('#history-dropdown').hide();
}

// 输入框获得焦点且为空时显示历史
$(document).on('focus', '#word', function() {
    if ($(this).val().trim() === '') {
        showHistoryDropdown();
    }
});

// 点击历史项
$(document).on('click', '#history-list li', function() {
    const word = $(this).data('word');
    $('#word').val(word);
    hideHistoryDropdown();
    queryMdx(word);
});

// 清空历史
$(document).on('click', '#clear-history', function(e) {
    e.stopPropagation();
    clearHistory();
});

// =============================================
// Loading 动画
// =============================================
function showLoading() {
    $('#mdx-resp').html(`
        <div class="loading-state">
            <div class="loading-spinner"></div>
            <p>查询中...</p>
        </div>
    `);
}

function showError(message) {
    $('#mdx-resp').html(`
        <div class="empty-state error-state">
            <div class="empty-state-icon"><svg class="icon"><use href="#icon-x-circle"/></svg></div>
            <p>${message}</p>
        </div>
    `);
}

function showEmpty() {
    $('#share-btn').hide();
    $('#mdx-resp').html(`
        <div class="empty-state">
            <div class="empty-state-icon"><svg class="icon"><use href="#icon-search"/></svg></div>
            <p>未找到相关词条</p>
        </div>
    `);
}

// =============================================
// 搜索高亮 & 一键复制
// =============================================
function highlightSearchTerm(html, term) {
    if (!term || term.length < 2) return html;

    // 创建临时 DOM 解析
    const $temp = $('<div>').html(html);

    // 在文本节点中高亮匹配片段，不影响 HTML 结构。
    // 注意：绝不能用 innerHTML 注入 nodeValue，否则像 "a<b>c" 的纯文本会被重新解析成标签，
    // 篡改甚至破坏词条内容。这里改用 splitText + mark.textContent 的安全拼接。
    const pattern = new RegExp(escapeRegExp(term), 'gi');

    const textNodes = [];
    const walker = document.createTreeWalker(
        $temp[0],
        NodeFilter.SHOW_TEXT,
        null,
        false
    );

    while (walker.nextNode()) {
        textNodes.push(walker.currentNode);
    }

    textNodes.forEach(node => {
        const text = node.nodeValue;
        if (!text || !pattern.test(text)) return;
        // .test() 会推进全局正则的 lastIndex，重置后再 exec。
        pattern.lastIndex = 0;

        const frag = document.createDocumentFragment();
        let last = 0;
        let match;
        while ((match = pattern.exec(text)) !== null) {
            if (match.index > last) {
                frag.appendChild(document.createTextNode(text.slice(last, match.index)));
            }
            const mark = document.createElement('mark');
            mark.className = 'search-highlight';
            mark.textContent = match[0];
            frag.appendChild(mark);
            last = match.index + match[0].length;
            // 防御零长度匹配导致死循环
            if (match.index === pattern.lastIndex) pattern.lastIndex++;
        }
        if (last < text.length) {
            frag.appendChild(document.createTextNode(text.slice(last)));
        }
        node.parentNode.replaceChild(frag, node);
    });

    return $temp.html();
}

function escapeRegExp(string) {
    return string.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

function safeDecodeURIComponent(value) {
    try {
        return decodeURIComponent(value);
    } catch (_) {
        return value;
    }
}

// 分享链接功能
$(document).on('click', '#share-btn', function(e) {
    e.preventDefault();
    const url = window.location.href;
    const $btn = $(this);
    const originalText = $btn.html();

    // 复制到剪贴板 (兼容 HTTP)
    function copyToClipboard(text) {
        // 方法1: 现代 Clipboard API (仅 HTTPS)
        if (navigator.clipboard && navigator.clipboard.writeText) {
            return navigator.clipboard.writeText(text);
        }

        // 方法2: 降级方案 - 使用临时 textarea
        return new Promise((resolve, reject) => {
            try {
                const textarea = document.createElement('textarea');
                textarea.value = text;
                textarea.style.position = 'fixed';
                textarea.style.opacity = '0';
                document.body.appendChild(textarea);
                textarea.select();
                document.execCommand('copy');
                document.body.removeChild(textarea);
                resolve();
            } catch (err) {
                reject(err);
            }
        });
    }

    copyToClipboard(url).then(() => {
        $btn.html('<svg class="icon"><use href="#icon-check"/></svg> 已复制');
        setTimeout(() => $btn.html(originalText), 2000);
    }).catch(err => {
        console.error('复制链接失败:', err);
        prompt('复制此链接分享:', url);
    });
});

// =============================================
// 核心查询功能
// =============================================
function queryMdx(word, updateHistory = true) {
    if (!word || !validInput(word)) {
        return;
    }

    currentQuery = word;
    showLoading();
    hideHistoryDropdown();
    hideSuggestions();

    // 更新 URL（用户主动查询时）
    if (!isNavigating) {
        updateUrl(word, updateHistory);
    }

    const queryData = { 'word': word };
    const dictsParam = buildDictsParam();
    if (dictsParam) queryData.dicts = dictsParam;

    $.ajax({
        url: './query',
        type: 'POST',
        data: queryData,
        dataType: 'html',
        success: function (data) {
            if (data && data.trim() !== '' && !data.includes('not found')) {
                // 保存到历史
                saveHistory(word);

                // 高亮搜索词
                let highlighted = highlightSearchTerm(data, currentQuery);

                $('#mdx-resp').html(highlighted).show();
                enhanceAggregateResult();

                // 添加复制按钮

                // 显示分享按钮
                $('#share-btn').show();

                // 更新页面标题
                document.title = word + ' - MDict 极速词典';
            } else {
                showEmpty();
            }
        },
        error: function(xhr) {
            if (xhr.status === 404) {
                showEmpty();
            } else {
                showError('查询出错，请稍后重试');
            }
        }
    });
}

function queryDictEntryByUrl(url, word) {
    if (!url) return;

    currentQuery = word || currentQuery;
    showLoading();
    hideHistoryDropdown();
    hideSuggestions();

    $.ajax({
        url: url,
        type: 'GET',
        dataType: 'html',
        success: function (data) {
            if (data && data.trim() !== '' && !data.includes('not found')) {
                if (word) {
                    saveHistory(word);
                    updateUrl(word, true);
                    $('#word').val(word);
                    document.title = word + ' - MDict 极速词典';
                }

                let highlighted = highlightSearchTerm(data, currentQuery);
                $('#mdx-resp').html(highlighted).show();
                enhanceAggregateResult();
                $('#share-btn').show();
            } else {
                showEmpty();
            }
        },
        error: function(xhr) {
            if (xhr.status === 404) {
                showEmpty();
            } else {
                showError('词条跳转失败，请稍后重试');
            }
        }
    });
}

function playAudioUrl(audioPath) {
    if (!audioPath) return;

    let audio = document.getElementById('mdict-audio');
    if (!audio) {
        audio = document.createElement('audio');
        audio.id = 'mdict-audio';
        document.body.appendChild(audio);
    }
    audio.src = audioPath;
    audio.play().catch(err => console.error('Audio play failed:', err));
}

function postQuery() {
    let word = $('#word').val().trim();
    if (!validInput(word)) {
        return;
    }
    queryMdx(word);
}

// 验证输入
function validInput(word) {
    return word
        && word.length > 0
        && word !== '.'
        && word !== '#'
        && word !== '?'
        && word !== '/';
}

// =============================================
// 联想词功能
// =============================================
function showSuggestions(suggestions) {
    let $list = $('#suggestions');
    $list.empty();
    hideHistoryDropdown();

    if (suggestions.length === 0) {
        $list.hide();
        return;
    }

    suggestions.forEach((word, index) => {
        // 高亮匹配部分
        const query = $('#word').val().trim().toLowerCase();
        let displayWord = word;
        if (word.toLowerCase().startsWith(query)) {
            displayWord = `<strong>${word.substring(0, query.length)}</strong>${word.substring(query.length)}`;
        }
        $list.append($('<li>').html(displayWord).data('word', word).data('index', index));
    });

    selectedIndex = -1;
    $list.show();
}

function hideSuggestions() {
    $('#suggestions').hide();
    selectedIndex = -1;
}

function selectSuggestion(index) {
    let $items = $('#suggestions li');
    $items.removeClass('selected');

    if (index >= 0 && index < $items.length) {
        $items.eq(index).addClass('selected');
        selectedIndex = index;
    } else {
        selectedIndex = -1;
    }
}

// 监听输入变化
$(document).on('input', '#word', function() {
    let query = $(this).val().trim();
    hideHistoryDropdown();

    if (suggestTimer) {
        clearTimeout(suggestTimer);
    }

    if (query.length < 2) {
        hideSuggestions();
        if (query.length === 0) {
            showHistoryDropdown();
        }
        return;
    }

    // 防抖
    suggestTimer = setTimeout(function() {
        const sugData = { q: query };
        const dParam = buildDictsParam();
        if (dParam) sugData.dicts = dParam;

        $.ajax({
            url: './suggest',
            type: 'GET',
            data: sugData,
            dataType: 'json',
            success: function(data) {
                showSuggestions(data);
            },
            error: function() {
                hideSuggestions();
            }
        });
    }, CONFIG.DEBOUNCE_MS);
});

// 点击建议项
$(document).on('click', '#suggestions li', function() {
    let word = $(this).data('word');
    $('#word').val(word);
    hideSuggestions();
    queryMdx(word);
});

// 点击其他地方隐藏下拉框
$(document).on('click', function(e) {
    if (!$(e.target).closest('#search-wrapper').length) {
        hideSuggestions();
        hideHistoryDropdown();
    }
});

// =============================================
// 键盘事件
// =============================================
$(document).on('keydown', '#word', function(e) {
    let $items = $('#suggestions li');
    let isVisible = $('#suggestions').is(':visible');

    if (e.keyCode === 13) { // Enter
        e.preventDefault();
        hideHistoryDropdown();
        if (isVisible && selectedIndex >= 0) {
            let word = $items.eq(selectedIndex).data('word');
            $('#word').val(word);
            hideSuggestions();
            queryMdx(word);
        } else {
            hideSuggestions();
            postQuery();
        }
    } else if (e.keyCode === 40 && isVisible) { // Down
        e.preventDefault();
        selectSuggestion(Math.min(selectedIndex + 1, $items.length - 1));
    } else if (e.keyCode === 38 && isVisible) { // Up
        e.preventDefault();
        selectSuggestion(Math.max(selectedIndex - 1, 0));
    } else if (e.keyCode === 27) { // Escape
        hideSuggestions();
        hideHistoryDropdown();
    }
});

// =============================================
// 链接点击处理
// =============================================
$(document).on('click', 'a', function (e) {
    let href = $(this).attr('href');
    if (!href) return;

    // 兼容旧 sound:// 协议
    if (href.startsWith('sound://')) {
        e.preventDefault();
        let audioPath = href.replace('sound://', '/');
        playAudioUrl(audioPath);
        return;
    }

    // 兼容旧 entry:// 协议
    if (href.startsWith('entry://')) {
        e.preventDefault();
        let word = safeDecodeURIComponent(href.slice('entry://'.length));
        if (word) {
            $('#word').val(word);
            queryMdx(word);
        }
        return;
    }

    let url;
    try {
        url = new URL(href, window.location.origin);
    } catch (_) {
        return;
    }

    if (url.origin !== window.location.origin) {
        return;
    }

    const path = url.pathname;
    const pathAndQuery = url.pathname + url.search + url.hash;

    // 新路由: /dict/{id}/entry/{word}
    const dictEntryMatch = path.match(/^\/dict\/[^/]+\/entry\/(.+)$/);
    if (dictEntryMatch) {
        e.preventDefault();
        let word = safeDecodeURIComponent(dictEntryMatch[1]);
        $('#word').val(word);
        queryDictEntryByUrl(pathAndQuery, word);
        return;
    }

    // 新路由: /dict/{id}/audio/{path}
    if (/^\/dict\/[^/]+\/audio\/.+/.test(path)) {
        e.preventDefault();
        playAudioUrl(pathAndQuery);
        return;
    }

    // 旧路由音频兼容: /resource/{id}/{path}
    if (/^\/resource\/[^/]+\/.+\.(mp3|wav|ogg|oga|flac|aac|m4a)$/i.test(path)) {
        e.preventDefault();
        playAudioUrl(pathAndQuery);
        return;
    }

    // 新路由资源直接放行，不拦截
    if (/^\/dict\/[^/]+\/res\/.+/.test(path)) {
        return;
    }

    // 兼容旧内部词条链接: /word
    if (path.startsWith('/') && !path.startsWith('/#') && !path.startsWith('/api/')) {
        e.preventDefault();
        let word = safeDecodeURIComponent(path.slice(1));
        if (word) {
            $('#word').val(word);
            queryMdx(word);
        }
    }
});

// =============================================
// 快捷键
// =============================================
$(window).bind('keyup keydown', function (e) {
    if ((e.ctrlKey || e.metaKey) && String.fromCharCode(e.which).toLowerCase() === 'l') {
        e.preventDefault();
        $('#word').val('').focus();
        scrollTo(0, 0);
        showHistoryDropdown();
    }
});

// =============================================
// 查询按钮（修复：空输入不查询）
// =============================================
$(document).on('click', '#lucky-btn', function (e) {
    let word = $('#word').val().trim();

    // 如果输入框有内容，执行查询
    if (word && validInput(word)) {
        queryMdx(word);
    } else {
        // 输入框为空时，执行"试试手气"
        $.ajax({
            url: './lucky',
            type: 'GET',
            dataType: 'html',
            success: function (data) {
                if (data && data.trim() !== '') {
                    $('#mdx-resp').html(data).show();
                    enhanceAggregateResult();
                } else {
                    showEmpty();
                }
            },
            error: function() {
                showError('获取随机词条失败');
            }
        });
    }
});
