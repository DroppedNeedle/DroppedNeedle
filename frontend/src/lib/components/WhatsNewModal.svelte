<script lang="ts">
	import { resolve } from '$app/paths';
	import { goto } from '$app/navigation';
	import { isWhatsNewDismissed, dismissWhatsNew } from '$lib/stores/version.svelte';
	import { renderMarkdown } from '$lib/utils/markdown';
	import { X, Sparkles, ExternalLink } from 'lucide-svelte';
	import type { GitHubRelease } from '$lib/queries/VersionQuery.svelte';

	interface Props {
		currentVersion: string | null;
		buildDate: string | null;
		releases: GitHubRelease[];
	}
	let { currentVersion, buildDate, releases }: Props = $props();

	let dialogEl: HTMLDialogElement | undefined = $state();
	let renderedSections: { tag: string; name: string | null; html: string }[] = $state([]);

	const isDev = $derived(currentVersion === 'dev' || currentVersion === 'hosting-local');
	const latestRelease = $derived(releases.length > 0 ? releases[0] : null);

	// In dev: key dismissal to build_date so modal shows once per rebuild, not every refresh
	// In prod: key to latest release tag so modal re-shows when a new patch lands
	const dismissKey = $derived(
		isDev ? (buildDate ?? 'dev') : (latestRelease?.tag_name ?? currentVersion)
	);

	const hasContent = $derived(releases.some((r) => r.body && r.body.trim().length > 0));

	const shouldShow = $derived(
		currentVersion !== null && dismissKey !== null && hasContent && !isWhatsNewDismissed(dismissKey)
	);

	$effect(() => {
		let aborted = false;
		const withContent = releases.filter((r) => r.body && r.body.trim());
		if (withContent.length === 0) {
			renderedSections = [];
			return;
		}

		Promise.all(
			withContent.map(async (r) => ({
				tag: r.tag_name,
				name: r.name,
				html: await renderMarkdown(r.body!)
			}))
		)
			.then((sections) => {
				if (!aborted) renderedSections = sections;
			})
			.catch(() => {
				if (!aborted) renderedSections = [];
			});

		return () => {
			aborted = true;
		};
	});

	$effect(() => {
		if (shouldShow && dialogEl && !dialogEl.open) {
			dialogEl.showModal();
		}
	});

	function handleDismiss() {
		if (dismissKey) {
			dismissWhatsNew(dismissKey);
		}
		dialogEl?.close();
	}

	function onDialogClose() {
		if (dismissKey && !isWhatsNewDismissed(dismissKey)) {
			dismissWhatsNew(dismissKey);
		}
	}

	function handleViewChangelog() {
		handleDismiss();
		goto(resolve('/settings?tab=about'));
	}
</script>

<dialog
	bind:this={dialogEl}
	class="modal whats-new-modal"
	onclose={onDialogClose}
	aria-labelledby="whats-new-title"
>
	<div
		class="modal-box whats-new-box max-w-2xl animate-fade-in-up border border-accent/10 p-5 sm:p-8"
	>
		<button
			class="whats-new-close absolute right-2 top-2 flex h-11 w-11 items-center justify-center rounded-lg text-base-content/60 transition-all duration-200 hover:bg-base-content/8 hover:text-base-content/80"
			onclick={handleDismiss}
			aria-label="Close"
		>
			<X class="h-3.5 w-3.5" />
		</button>

		<div class="mb-6 flex items-center gap-4">
			<div
				class="whats-new-icon-wrap relative flex h-11 w-11 items-center justify-center rounded-xl bg-accent/10 animate-glow-pulse"
			>
				<Sparkles class="text-accent h-5 w-5" />
			</div>
			<div>
				<h3 id="whats-new-title" class="text-xl font-bold tracking-tight">What's New</h3>
				{#if currentVersion}
					<p class="text-primary/70 mt-0.5 text-xs font-medium tracking-wide uppercase">
						{currentVersion}
					</p>
				{/if}
			</div>
		</div>

		<div class="divider my-0 opacity-10"></div>

		{#if renderedSections.length > 0}
			<div
				class="whats-new-content release-notes-prose max-h-[55vh] max-w-none overflow-y-auto rounded-lg border border-base-content/5 bg-base-100/50 p-4 mt-4"
			>
				{#each renderedSections as section, i (section.tag)}
					{#if i > 0}
						<div class="divider my-4 opacity-20"></div>
					{/if}
					<p
						class="text-base-content text-sm font-semibold border-l-2 border-accent/50 pl-3 {i > 0
							? ''
							: 'mt-0'} mb-3"
					>
						{section.name ?? section.tag}
					</p>
					<div class="prose prose-sm max-w-none text-base-content/75">
						<!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized via DOMPurify -->
						{@html section.html}
					</div>
				{/each}
			</div>
		{:else if hasContent}
			<div class="flex justify-center py-12">
				<span class="loading loading-spinner loading-md text-accent/60"></span>
			</div>
		{/if}

		<div class="modal-action mt-6 gap-3">
			<button
				class="btn btn-ghost btn-sm text-base-content/60 hover:text-base-content/80 gap-1.5"
				onclick={handleViewChangelog}
			>
				View full changelog
				<ExternalLink class="h-3 w-3" />
			</button>
			<button class="btn btn-accent btn-sm px-6 shadow-sm shadow-accent/15" onclick={handleDismiss}
				>Got it</button
			>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button>close</button>
	</form>
</dialog>

<style>
	.whats-new-box {
		background:
			radial-gradient(
				ellipse at top left,
				oklch(from var(--color-accent) l c h / 0.04),
				transparent 50%
			),
			radial-gradient(
				ellipse at bottom right,
				oklch(from var(--color-primary) l c h / 0.03),
				transparent 50%
			),
			var(--color-base-200);
		box-shadow:
			0 0 0 1px oklch(from var(--color-accent) l c h / 0.06),
			0 24px 80px -12px rgba(0, 0, 0, 0.6);
	}

	.whats-new-content {
		scrollbar-width: thin;
		scrollbar-color: oklch(from var(--color-accent) l c h / 0.15) transparent;
	}
</style>
