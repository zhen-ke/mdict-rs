var d8018d6852bc49e3b3e655364cf1439c = {
    // ==================== 原代码 ====================
    toggle: function (expandable) {
        var expParent = expandable.parentNode;
        var img = expParent.querySelector('img');
        if (img !== null) {
            if (img.addEventListener) {
                img.addEventListener('click', function (event) {
                    event.stopPropagation();
                });
            } else if (img.attachEvent) {
                img.attachEvent('onclick', function (event) {
                    event.cancelBubble = true;
                });
            }
        }
        var target = expParent.querySelector('.content');
        if (target !== null) {
            if (target.addEventListener) {
                target.addEventListener('click', function (event) {
                    event.stopPropagation();
                });
            } else if (target.attachEvent) {
                target.attachEvent('onclick', function (event) {
                    event.cancelBubble = true;
                });
            }
        }
        var arrow = expParent.querySelector('span.arrow');
        if (arrow !== null) {
            if (target.style.display == 'block') {
                target.style.display = 'none';
                arrow.innerHTML = '\u25BA';
            } else {
                target.style.display = 'block';
                arrow.innerHTML = '\u25BC';
            }
        }

        this.processContent();
    },

    showAtLink: function (ele) {
        if (ele.nextSibling.style.display == 'block') {
            ele.nextSibling.style.display = 'none';
        } else {
            ele.nextSibling.style.display = 'block';
        }
    },

    toggleImg: function (ele) {
        if (ele.style.maxHeight == 'none') {
            ele.style.maxHeight = '4em';
        } else {
            ele.style.maxHeight = 'none';
        }
    },
    // ==================== 新代码 ====================
    // 统一判断元素是否折叠
    isCollapsed: function(element) {
        if (!element) return true;
        if (element.hasAttribute('data-collapsed')) {
            return element.getAttribute('data-collapsed') === 'true';
        }
        // 初始状态判断
        var computedStyle = window.getComputedStyle(element);
        return computedStyle.display === 'none';
    },

    // 统一设置元素折叠状态
    setCollapsed: function(element, collapsed) {
        if (!element) return;
        element.setAttribute('data-collapsed', collapsed);
        element.style.display = collapsed ? 'none' : '';
    },

    processContent: function() {
        // 移除 .hyphenation, .arrow, .colloexa .collo 中的音节划分符号和重音符号
        var elements = document.querySelectorAll('.hyphenation, .arrow, .colloexa .collo');
        Array.prototype.forEach.call(elements, function(el) {
            el.innerHTML = el.innerHTML.replace(/[·‧ˈˌ]/g, '');
        });

        // 移除 .entry 中的音节符号，但保留其内部的 .pron 和 .amevarpron 中的符号
        var entries = document.querySelectorAll('.entry');
        Array.prototype.forEach.call(entries, function(entry) {
            var walker = document.createTreeWalker(entry, NodeFilter.SHOW_TEXT, null, false);
            var node;

            while (node = walker.nextNode()) {
                // 检查当前文本节点是否在 .pron 或 .amevarpron 内部
                var parent = node.parentNode;
                var isInsidePron = parent.classList &&
                                  (parent.classList.contains('pron') ||
                                   parent.classList.contains('amevarpron'));

                // 如果不在 .pron/.amevarpron 内，则移除音节符号
                if (!isInsidePron) {
                    node.nodeValue = node.nodeValue.replace(/[·‧ˈˌ]/g, '');
                }
            }
        });

        // 移除 span.neutral 中的冒号、斜杠和美元符号，并将 → 替换为蓝色 ►
        var neutrals = document.querySelectorAll('span.neutral');
        Array.prototype.forEach.call(neutrals, function(el) {
            el.innerHTML = el.innerHTML
                .replace(/[:/\$]/g, '') // 移除特殊符号
                .replace(/→/g, '<span style="color:#6dbdff;">►</span>'); // 替换箭头
        });

        // 处理音标部分
        var prons = document.querySelectorAll('.pron, .amevarpron');
        Array.prototype.forEach.call(prons, function(el) {
            el.innerHTML = el.innerHTML.replace(/[◂]/g, ''); // 删除音节符号
            el.innerHTML = el.innerHTML.replace(/^\/|\/$|\s+/g, ''); // 移除首尾斜杠和多余空格
            el.innerHTML = el.innerHTML.replace(/,(\S)/g, ', $1'); // 处理逗号：在逗号后添加空格

            // 重新添加斜杠
            if (el.classList.contains('pron')) {
                el.innerHTML = '/' + el.innerHTML + '/';
            } else if (el.classList.contains('amevarpron')) {
                el.innerHTML = '/' + el.innerHTML + '/';
            }
        });

        // 移除 .lexunit 前的所有空格
        var lexunits = document.querySelectorAll('.lexunit');
        Array.prototype.forEach.call(lexunits, function(el) {
            // 移除开头的普通空格、&nbsp; 和换行符
            el.innerHTML = el.innerHTML.replace(/^(\s|&nbsp;|\n)+/, '');

            // 处理前一个兄弟节点中的空格
            if (el.previousSibling && el.previousSibling.nodeType === 3) {
                el.previousSibling.textContent = el.previousSibling.textContent.replace(/\s+$/, '');
            }
        });

        // 针对 span.synopp 中 "GRAMMAR" 文本的特殊样式调整
        var synopps = document.querySelectorAll('span.synopp');
        Array.prototype.forEach.call(synopps, function(el) {
            if (el.textContent.includes('GRAMMAR')) {
                el.style.marginRight = '4px';
                el.style.marginLeft = '0px';
                el.style.color = '#83b5de';
                el.style.borderColor = '#83b5de';
                el.style.position = 'relative';
                el.style.top = '-0.5px';
            }
        });

        // 没序号的释义前添加“❑”
        var defs = document.querySelectorAll('.def');
        Array.prototype.forEach.call(defs, function(def) {
            // 检查当前def是否在五种知识框中
            var isInBox = false;
            var parent = def.parentElement;

            // 向上遍历父元素，检查是否在知识框中
            while (parent) {
                if (parent.classList &&
                    (parent.classList.contains('collobox') ||
                     parent.classList.contains('thesbox') ||
                     parent.classList.contains('usagebox') ||
                     parent.classList.contains('grambox') ||
                     parent.classList.contains('f2nbox'))) {
                    isInBox = true;
                    break;
                }
                parent = parent.parentElement;
            }

            // 如果在知识框中，跳过处理
            if (isInBox) return;

            // 检查当前def之前的所有兄弟节点中是否有sensenum
            var hasSensenum = false;
            var prev = def.previousElementSibling;

            // 向前遍历所有兄弟节点
            while (prev) {
                if (prev.classList.contains('sensenum')) {
                    hasSensenum = true;
                    break;
                }
                prev = prev.previousElementSibling;
            }

            // 如果没有找到sensenum，则添加❑
            if (!hasSensenum) {
                // 确保没有重复添加
                if (!def.previousElementSibling ||
                    !def.previousElementSibling.classList.contains('def-marker')) {

                    // 创建❑元素
                    var square = document.createElement('span');
                    square.className = 'def-marker';
                    square.innerHTML = '❑';
                    square.style.fontSize = '0.9em';
                    square.style.color = '#ed1941';
                    square.style.display = 'inline';

                    // 插入到def所在容器的最前面
                    var container = def.parentNode;
                    container.insertBefore(square, container.firstChild);
                }
            }
        });

        // 去除某些.registerlab前的空格
        var registerlabs = document.querySelectorAll('.registerlab');
        Array.prototype.forEach.call(registerlabs, function(el) {
            // 遍历所有子节点
            var walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT, null, false);
            var node;

            while (node = walker.nextNode()) {
                // 删除所有空格（包括普通空格、&nbsp;等）
                node.nodeValue = node.nodeValue.replace(/[\s\u00A0]+/g, '');
            }
        });
    },

    // 知识框折叠功能
    initBoxToggle: function() {
        var boxes = document.querySelectorAll('.collobox, .thesbox, .usagebox, .grambox, .f2nbox');

        Array.prototype.forEach.call(boxes, function(box) {
            var heading = box.querySelector('.heading');

            if (heading) {
                var contentAreas = box.querySelectorAll(
                    '.section, .expl, .content, .collocations, .grammar, ' +
                    '.thesaurus, .item, .group, .sense, .subsense, .example, ' +
                    '.collocate, .expcn, .explcn, .collocn, .gramcn, ' +
                    '.compareword, .gramexa, .gramrefcont'
                );

                heading.addEventListener('click', function() {
                    var isAnyVisible = false;

                    Array.prototype.forEach.call(contentAreas, function(area) {
                        if (!d8018d6852bc49e3b3e655364cf1439c.isCollapsed(area)) {
                            isAnyVisible = true;
                        }
                    });

                    Array.prototype.forEach.call(contentAreas, function(area) {
                        d8018d6852bc49e3b3e655364cf1439c.setCollapsed(area, isAnyVisible);
                    });
                });

                // 添加手形光标提示可点击
                heading.style.cursor = 'pointer';

                // 初始状态设为折叠
                Array.prototype.forEach.call(contentAreas, function(area) {
                    d8018d6852bc49e3b3e655364cf1439c.setCollapsed(area, true);
                });
            }
        });
    },

    // 获取需要折叠/展开的目标元素（排除知识框内部元素和.def）
    getToggleTargets: function(container) {
        var targets = [];
        var walker = document.createTreeWalker(
            container,
            NodeFilter.SHOW_ELEMENT,
            {
                acceptNode: function(node) {
                    // 跳过知识框及其内部元素
                    if (node.closest('.collobox, .thesbox, .usagebox, .grambox, .f2nbox')) {
                        return NodeFilter.FILTER_REJECT;
                    }

                    // 跳过.def元素
                    if (node.classList && node.classList.contains('def')) {
                        return NodeFilter.FILTER_SKIP;
                    }

                    // 匹配目标元素
                    if (node.classList &&
                        (node.classList.contains('example') ||
                         node.classList.contains('expcn') ||
                         node.classList.contains('colloexa') ||
                         node.classList.contains('propform') ||
                         node.classList.contains('propformprep') ||
                         node.classList.contains('thesref') ||
                         node.classList.contains('reflex') ||
                         node.classList.contains('expl') ||
                         node.classList.contains('explcn') ||
                         node.classList.contains('collocn') ||
                         node.classList.contains('gramcn'))) {
                        return NodeFilter.FILTER_ACCEPT;
                    }
                    return NodeFilter.FILTER_SKIP;
                }
            },
            false
        );

        while (walker.nextNode()) {
            targets.push(walker.currentNode);
        }

        return targets;
    },

    // 统一更新所有目标元素的显示状态
    updateTargetsDisplay: function(container, isCollapsed) {
        var targets = this.getToggleTargets(container);
        var self = this;
        Array.prototype.forEach.call(targets, function(target) {
            self.setCollapsed(target, isCollapsed);
        });
    },

    // 切换词头下的所有示例
    toggleAllExamplesUnderHyphenation: function(hyphenation) {
        var container = hyphenation.closest('.entry');
        if (!container) return;

        // 获取当前折叠状态
        var currentTarget = this.getToggleTargets(container)[0];
        var currentlyCollapsed = this.isCollapsed(currentTarget);

        // 切换状态
        this.updateTargetsDisplay(container, !currentlyCollapsed);
    },

    // 词头.hyphenation折叠控制
    initHyphenationToggle: function() {
        var self = this;
        var hyphenations = document.querySelectorAll('.hyphenation');

        Array.prototype.forEach.call(hyphenations, function(hyphenation) {
            // 查找需要控制折叠的元素
            var container = hyphenation.closest('.entry');
            if (!container) return;

            // 设置手形光标
            hyphenation.style.cursor = 'pointer';

            // 初始状态设为折叠
            self.updateTargetsDisplay(container, true);

            // 长按相关状态变量
            var isLongPress = false;
            var longPressTimer;
            var hasTextSelection = false;

            // 鼠标/触摸按下事件
            hyphenation.addEventListener('mousedown', function(e) {
                isLongPress = false;
                hasTextSelection = false;
                longPressTimer = setTimeout(function() {
                    isLongPress = true;
                }, 200);
            });

            hyphenation.addEventListener('touchstart', function(e) {
                isLongPress = false;
                hasTextSelection = false;
                longPressTimer = setTimeout(function() {
                    isLongPress = true;
                }, 200);
            }, {passive: true});

            // 鼠标/触摸释放事件
            hyphenation.addEventListener('mouseup', function(e) {
                clearTimeout(longPressTimer);
                // 检查是否有文本被选中
                var selection = window.getSelection();
                hasTextSelection = selection.toString().length > 0;

                // 如果是长按或有文本选择，则阻止默认行为
                if (isLongPress || hasTextSelection) {
                    e.preventDefault();
                    return;
                }

                // 否则触发切换
                self.toggleAllExamplesUnderHyphenation(this);
            });

            hyphenation.addEventListener('touchend', function(e) {
                clearTimeout(longPressTimer);
                // 检查是否有文本被选中
                var selection = window.getSelection();
                hasTextSelection = selection.toString().length > 0;

                // 如果是长按或有文本选择，则阻止默认行为
                if (isLongPress || hasTextSelection) {
                    e.preventDefault();
                    return;
                }

                // 否则触发切换
                self.toggleAllExamplesUnderHyphenation(this);
            });

            hyphenation.addEventListener('mouseleave', function() {
                clearTimeout(longPressTimer);
            });
        });
    },

    // ❑符号折叠功能
    initSquareToggle: function() {
        var self = this;
        var squares = document.querySelectorAll('.def-marker');

        Array.prototype.forEach.call(squares, function(square) {
            // 查找需要控制折叠的元素
            var container = square.parentNode;
            var targets = self.getToggleTargets(container);

            // 如果没有找到目标元素，取消触发机制
            if (targets.length === 0) {
                square.style.cursor = '';
                return;
            }

            // 设置手形光标
            square.style.cursor = 'pointer';

            // 初始状态设为折叠
            Array.prototype.forEach.call(targets, function(target) {
                self.setCollapsed(target, true);
            });

            // 添加点击事件
            square.addEventListener('click', function(e) {
                e.stopPropagation();
                var firstTarget = targets[0];
                var currentlyCollapsed = self.isCollapsed(firstTarget);

                Array.prototype.forEach.call(targets, function(target) {
                    self.setCollapsed(target, !currentlyCollapsed);
                });
            });
        });
    },

    // 释义序号折叠功能
    initSensenumToggle: function() {
        var self = this;
        var sensenums = document.querySelectorAll('span.sensenum');

        Array.prototype.forEach.call(sensenums, function(sensenum) {
            // 查找需要控制折叠的元素
            var container = sensenum.parentNode;
            var targets = self.getToggleTargets(container);

            // 如果没有找到目标元素，取消触发机制
            if (targets.length === 0) {
                sensenum.style.cursor = '';
                return;
            }

            // 设置手形光标
            sensenum.style.cursor = 'pointer';

            // 初始状态设为折叠
            Array.prototype.forEach.call(targets, function(target) {
                self.setCollapsed(target, true);
            });

            // 添加点击事件
            sensenum.addEventListener('click', function(e) {
                e.stopPropagation();
                var firstTarget = targets[0];
                var currentlyCollapsed = self.isCollapsed(firstTarget);

                Array.prototype.forEach.call(targets, function(target) {
                    self.setCollapsed(target, !currentlyCollapsed);
                });
            });
        });
    },

    // .deriv元素折叠功能
    initDerivToggle: function() {
        var self = this;
        var derivs = document.querySelectorAll('.deriv');

        Array.prototype.forEach.call(derivs, function(deriv) {
            // 查找需要控制折叠的元素
            var container = deriv.parentNode;
            var targets = self.getToggleTargets(container);

            // 如果没有找到目标元素，取消触发机制
            if (targets.length === 0) {
                deriv.style.cursor = '';
                return;
            }

            // 设置手形光标
            deriv.style.cursor = 'pointer';

            // 初始状态设为折叠
            Array.prototype.forEach.call(targets, function(target) {
                self.setCollapsed(target, true);
            });

            // 添加点击事件
            deriv.addEventListener('click', function(e) {
                e.stopPropagation();
                var firstTarget = targets[0];
                var currentlyCollapsed = self.isCollapsed(firstTarget);

                Array.prototype.forEach.call(targets, function(target) {
                    self.setCollapsed(target, !currentlyCollapsed);
                });
            });
        });
    },

    // 添加悬停效果
    initHoverEffects: function() {
        // 词头.hyphenation悬停效果
        var hyphenations = document.querySelectorAll('.hyphenation');
        Array.prototype.forEach.call(hyphenations, function(el) {
            // 检查是否有折叠功能（通过查找关联的可折叠元素）
            var container = el.closest('.entry');
            // var hasToggle = container && this.getToggleTargets(container).length > 0;
            // if (hasToggle) {
            //     el.addEventListener('mouseenter', function() {
            //         this.style.color = '#1e45b0'; // 悬停时变为深蓝
            //     });
            //     el.addEventListener('mouseleave', function() {
            //         this.style.color = '#ff5050'; // 恢复默认颜色
            //     });
            // }
        }.bind(this));

        // ❑符号悬停效果
        var squares = document.querySelectorAll('.def-marker');
        Array.prototype.forEach.call(squares, function(el) {
            // 检查是否有折叠功能（通过查找关联的可折叠元素）
            var container = el.parentNode;
            var hasToggle = container && this.getToggleTargets(container).length > 0;
            if (hasToggle) {
                el.addEventListener('mouseenter', function() {
                    this.style.color = '#1e45b0'; // 悬停时变为瞎眼蓝
                });
                el.addEventListener('mouseleave', function() {
                    this.style.color = '#ed1941'; // 恢复红色
                });
            }
        }.bind(this));

        // 释义序号span.sensenum悬停效果
        var sensenums = document.querySelectorAll('span.sensenum');
        Array.prototype.forEach.call(sensenums, function(el) {
            // 检查是否有折叠功能（通过查找关联的可折叠元素）
            var container = el.parentNode;
            var hasToggle = container && this.getToggleTargets(container).length > 0;
            if (hasToggle) {
                el.addEventListener('mouseenter', function() {
                    this.style.color = '#1e45b0'; // 悬停时变为瞎眼蓝
                });
                el.addEventListener('mouseleave', function() {
                    this.style.color = '#ed1941'; // 恢复红色
                });
            }
        }.bind(this));

        // .deriv元素悬停效果
        var derivs = document.querySelectorAll('.deriv');
        Array.prototype.forEach.call(derivs, function(el) {
            // 检查是否有折叠功能（通过查找关联的可折叠元素）
            var container = el.parentNode;
            var hasToggle = container && this.getToggleTargets(container).length > 0;
            if (hasToggle) {
                el.addEventListener('mouseenter', function() {
                    this.style.color = '#1e45b0'; // 悬停时变为瞎眼蓝
                });
                el.addEventListener('mouseleave', function() {
                    this.style.color = '#1d2a57'; // 恢复默认颜色
                });
            }
        }.bind(this));
    },

    // 初始化函数
    init: function() {
        var self = this;
        if (document.readyState === 'complete' || document.readyState === 'interactive') {
            self.processContent();
            self.initBoxToggle();
            self.initHyphenationToggle();
            self.initSquareToggle();
            self.initSensenumToggle();
            self.initDerivToggle();
            self.initHoverEffects();
        } else {
            document.addEventListener('DOMContentLoaded', function() {
                self.processContent();
                self.initBoxToggle();
                self.initHyphenationToggle();
                self.initSquareToggle();
                self.initSensenumToggle();
                self.initDerivToggle();
                self.initHoverEffects();
            });
        }
    }
};

// 执行初始化
d8018d6852bc49e3b3e655364cf1439c.init();
