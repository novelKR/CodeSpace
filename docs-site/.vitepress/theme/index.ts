// SPDX-License-Identifier: MIT
import DefaultTheme from 'vitepress/theme-without-fonts';
import { h } from 'vue';
import { useData, withBase } from 'vitepress';
import PageTools from './PageTools.vue';
import './tokens.css';

const repository = 'https://github.com/novelKR/CodeSpace';

export default {
  extends: DefaultTheme,
  Layout: {
    setup() {
      const { lang, frontmatter } = useData();
      return () => h(DefaultTheme.Layout, null, {
        'doc-before': () => h(PageTools),
        'doc-footer-before': () => {
          const ko = lang.value.startsWith('ko');
          const commit = frontmatter.value.sourceCommit;
          return h('nav', { class: 'document-meta', 'aria-label': ko ? '문서 출처와 고지' : 'Document source and notices' }, [
            h('a', { href: repository + '/commit/' + commit }, (ko ? '소스 · ' : 'Source · ') + commit?.slice(0, 7)),
            h('a', { href: withBase('/web-notices.txt') }, ko ? '웹 의존성 고지' : 'Web dependency notices'),
            h('a', { href: withBase('/LICENSE.txt') }, 'Apache-2.0'),
          ]);
        },
        'home-features-after': () => {
          const ko = lang.value.startsWith('ko');
          const labels = ko ? ['작업 공간 등록', 'MCP 연결', '읽기·수정·실행', '결과 확인'] : ['Register a workspace', 'Connect MCP', 'Read, edit, run', 'Inspect results'];
          return h('section', { class: 'deployment-path', 'aria-label': ko ? 'CodeSpace 사용 순서' : 'CodeSpace workflow' }, [
            h('p', { class: 'deployment-caption' }, ko ? '에이전트가 작업을 판단하고, CodeSpace가 실행합니다.' : 'Your agent directs the work. CodeSpace executes it.'),
            h('ol', labels.map((text, index) => h('li', [h('span', { class: 'step-number', 'aria-hidden': 'true' }, String(index + 1).padStart(2, '0')), text]))),
          ]);
        },
      });
    },
  },
};
