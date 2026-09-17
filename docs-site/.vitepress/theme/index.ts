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
          const labels = ko ? ['빌드', '검증', '산출물', '게시'] : ['Build', 'Verify', 'Artifact', 'Publish'];
          return h('section', { class: 'deployment-path', 'aria-label': ko ? '배포 순서' : 'Deployment steps' }, [
            h('p', { class: 'deployment-caption' }, ko ? '한 번 검증한 산출물로 게시합니다.' : 'Publish the artifact you verified.'),
            h('ol', labels.map((text, index) => h('li', [h('span', { class: 'step-number', 'aria-hidden': 'true' }, String(index + 1).padStart(2, '0')), text]))),
          ]);
        },
      });
    },
  },
};
