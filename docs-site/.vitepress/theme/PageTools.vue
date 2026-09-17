<!-- SPDX-License-Identifier: MIT -->
<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useData, useRoute, withBase } from 'vitepress';
const { frontmatter } = useData();
const route = useRoute();
const ko = computed(() => frontmatter.value.docLocale === 'ko');
const status = ref('');
const busy = ref(false);
watch(() => route.path, () => { status.value = ''; });
async function copyPage() {
  const current = route.path;
  busy.value = true;
  try {
    const response = await fetch(withBase(frontmatter.value.copyPath));
    if (!response.ok) throw new Error('Markdown is unavailable');
    const text = await response.text();
    await navigator.clipboard.writeText(text);
    if (route.path === current) status.value = ko.value ? '원문을 복사했습니다.' : 'Original Markdown copied.';
  } catch {
    if (route.path === current) status.value = ko.value ? '복사하지 못했습니다. 브라우저 권한을 확인하세요.' : 'Copy failed. Check browser permissions.';
  } finally { busy.value = false; }
}
</script>
<template>
  <div v-if="frontmatter.copyPath" class="page-tools">
    <span class="page-tools-label">{{ ko ? '문서' : 'Documentation' }}</span>
    <div class="page-tools-actions">
      <button type="button" :disabled="busy" :title="ko ? '이 페이지를 Markdown으로 복사' : 'Copy this page as Markdown'" @click="copyPage">
        <svg aria-hidden="true" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6"><rect x="8" y="8" width="12" height="13" rx="2"/><path d="M16 8V5a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h3"/></svg>
        {{ busy ? (ko ? '복사 중…' : 'Copying…') : (ko ? '페이지 복사' : 'Copy page') }}
      </button>
    </div>
    <span v-if="status" class="page-tools-status" role="status" aria-live="polite">{{ status }}</span>
  </div>
</template>
