// SPDX-License-Identifier: MIT
import { defineConfig } from 'vitepress';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { notices } from '../scripts/notices.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));
const catalog = JSON.parse(readFileSync(root + '.local/docs-site/catalogue.json', 'utf8'));
const repository = 'https://github.com/novelKR/CodeSpace';
const base = '/CodeSpace/';

function sidebar(ko: boolean) {
  const locale = ko ? 'ko' : 'en';
  const groups = new Map<string, { text: string; items: { text: string; link: string }[] }>();
  for (const group of catalog.groups) {
    groups.set(group.id, { text: ko ? group.ko : group.en, items: [] });
  }
  const pages = catalog.pages
    .filter((page: { locale: string }) => page.locale === locale)
    .sort((a: { order: number }, b: { order: number }) => a.order - b.order);
  for (const page of pages) {
    const group = groups.get(page.section);
    if (group) group.items.push({ text: page.title, link: page.route });
  }
  return [...groups.values()].filter((group) => group.items.length > 0);
}

function locale(ko: boolean) {
  const prefix = ko ? '/ko' : '';
  return {
    label: ko ? '한국어' : 'English', lang: ko ? 'ko-KR' : 'en-US', link: prefix + '/',
    description: ko
      ? '클라이언트가 판단하고, CodeSpace가 읽고 패치하고 실행합니다'
      : 'The client decides. CodeSpace reads, patches, and runs.',
    themeConfig: {
      nav: [
        { text: ko ? '문서' : 'Docs', link: prefix + '/guide/getting-started' },
        { text: ko ? '프로젝트' : 'Project', items: [
          { text: ko ? '문서 사이트' : 'Documentation site', link: prefix + '/guide/documentation' },
          { text: 'GitHub', link: repository },
        ] },
        { text: 'GitHub', link: repository },
      ],
      sidebar: sidebar(ko),
      footer: {
        message: '<a href="' + base + 'LICENSE.txt">Apache-2.0</a> · <a href="' + base + 'web-notices.txt">' + (ko ? '웹 의존성 고지' : 'Web dependency notices') + '</a>',
        copyright: (ko ? '소스 · ' : 'Source · ') + '<a href="' + repository + '/commit/' + catalog.source_commit + '">' + catalog.source_commit.slice(0, 7) + '</a>',
      },
      outline: { level: [2, 3], label: ko ? '이 페이지에서' : 'On this page' },
      docFooter: { prev: ko ? '이전' : 'Previous', next: ko ? '다음' : 'Next' },
      sidebarMenuLabel: ko ? '메뉴' : 'Menu', returnToTopLabel: ko ? '맨 위로' : 'Return to top',
      darkModeSwitchLabel: ko ? '테마' : 'Appearance', langMenuLabel: ko ? '언어 선택' : 'Change language',
      lightModeSwitchTitle: ko ? '밝은 테마로 전환' : 'Switch to light theme',
      darkModeSwitchTitle: ko ? '어두운 테마로 전환' : 'Switch to dark theme',
      skipToContentLabel: ko ? '본문으로 건너뛰기' : 'Skip to content',
      notFound: { title: ko ? '페이지를 찾을 수 없습니다' : 'PAGE NOT FOUND',
        quote: ko ? '메뉴 또는 검색에서 문서를 찾아보세요.' : 'Find a document using navigation or search.',
        linkText: ko ? '홈으로 돌아가기' : 'Take me home', linkLabel: ko ? '홈' : 'Home' },
    },
  };
}
export default defineConfig({
  title: 'CodeSpace', base,
  srcDir: '../.local/docs-site/source', srcExclude: ['public/**'], outDir: '../.local/docs-site/dist',
  cacheDir: '../.local/docs-site/cache', cleanUrls: true, buildConcurrency: 1,
  locales: { root: locale(false), ko: locale(true) },
  themeConfig: {
    search: { provider: 'local', options: { locales: { ko: { translations: {
      button: { buttonText: '검색', buttonAriaLabel: '문서 검색' },
      modal: { noResultsText: '검색 결과가 없습니다', resetButtonTitle: '검색 초기화',
        footer: { selectText: '선택', navigateText: '이동', closeText: '닫기' } },
    } } } } },
  },
  vite: { resolve: { alias: [
    { find: /^vue$/, replacement: root + 'docs-site/node_modules/vue/dist/vue.runtime.esm-bundler.js' },
    { find: /^vue\/server-renderer$/, replacement: root + 'docs-site/node_modules/vue/server-renderer/index.mjs' },
  ] }, plugins: [notices(root)], build: { sourcemap: false } },
});
