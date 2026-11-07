<!--
	The bar along the top: logo, search, service health, the theme toggle and the
	profile link. On phones the search field gives way to an icon that opens the
	search dialog, so the logo keeps its full width, except on the search
	pages, where the field stays with the query in it.
-->
<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { albumHref, artistHref } from '$lib/utils/entityRoutes';
	import { Moon, MoonStar, Search, Sun, UserRound } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Logo from '$lib/ui/Logo.svelte';
	import { part } from '$lib/ui/parts';
	import SearchSuggestions from '$lib/features/search/components/SearchSuggestions.svelte';
	import ServiceHealthIndicator from '$lib/shell/ServiceHealthIndicator.svelte';
	import { getApiUrl } from '$lib/api/api-utils';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { themeStore } from '$lib/stores/theme.svelte';
	import { withBasePath, withoutBasePath } from '$lib/utils/basePath';
	import type { SuggestResult } from '$lib/features/search/queries/types';

	interface Props {
		/** Opens the search dialog (the phone bar's search icon). */
		onsearch: () => void;
	}

	let { onsearch }: Props = $props();

	let query = $state('');

	const onSearchPage = $derived(/^\/search(\/|$)/.test(withoutBasePath(page.url.pathname)));

	// A search page's query shows in the field, so it can be changed in place.
	$effect(() => {
		if (onSearchPage) query = page.url.searchParams.get('q') ?? '';
	});

	const theme = $derived(themeStore.resolved);
	const themeLabel = $derived(
		theme === 'dark'
			? 'Switch to midnight theme'
			: theme === 'midnight'
				? 'Switch to light theme'
				: 'Switch to dark theme'
	);

	function toggleTheme() {
		themeStore.set(theme === 'light' ? 'dark' : theme === 'dark' ? 'midnight' : 'light');
	}

	function handleSearch() {
		if (query.trim()) {
			goto(withBasePath(`/search?q=${encodeURIComponent(query)}`));
		}
	}

	function handleSelect(result: SuggestResult) {
		goto(
			result.type === 'artist'
				? artistHref(result.musicbrainz_id)
				: albumHref(result.musicbrainz_id)
		);
	}
</script>

{#snippet themeGlyph(size: string)}
	<span class="theme-glyph" aria-hidden="true">
		<span class="theme-glyph__face" class:is-away={theme !== 'dark'}>
			<Moon class={size} />
		</span>
		<span class="theme-glyph__face theme-glyph__face--sun" class:is-away={theme !== 'light'}>
			<Sun class={size} />
		</span>
		<span
			class="theme-glyph__face theme-glyph__face--midnight"
			class:is-away={theme !== 'midnight'}
		>
			<MoonStar class={size} />
		</span>
	</span>
{/snippet}

<div
	class="droppedneedle-topbar navbar sticky top-0 z-50 border-b border-base-300 bg-base-100"
	data-part={part('top-bar')}
>
	<div class="navbar-start w-auto sm:grow-0" class:grow={!onSearchPage}>
		<Button variant="ghost" href={withBasePath('/')} class="px-2 sm:px-3" aria-label="Home">
			{#if onSearchPage}
				<!-- a narrow phone keeps the search field's room: the mark alone -->
				<Logo label="" class="h-7 text-base-content max-[359px]:hidden" />
				<Logo variant="icon" label="" class="h-7 text-base-content min-[360px]:hidden" />
			{:else}
				<Logo label="" class="h-7 text-base-content" />
			{/if}
		</Button>
	</div>
	<div
		class="navbar-center min-w-0 shrink grow basis-0 justify-center px-1 sm:flex sm:px-4"
		class:hidden={!onSearchPage}
		class:flex={onSearchPage}
	>
		<div class="w-full max-w-2xl">
			<SearchSuggestions
				bind:query
				onSearch={handleSearch}
				onSelect={handleSelect}
				id="navbar-suggest"
			/>
		</div>
	</div>
	<div class="navbar-end w-auto gap-1 pr-1 sm:pr-2">
		{#if !onSearchPage}
			<IconButton label="Search" icon={Search} class="sm:hidden" onclick={onsearch} />
		{/if}
		<ServiceHealthIndicator />
		<IconButton label={themeLabel} glyph={themeGlyph} onclick={toggleTheme} />
		<IconButton
			href={withBasePath('/profile')}
			label="Profile"
			icon={UserRound}
			image={authStore.user?.avatar_url ? getApiUrl(authStore.user.avatar_url) : null}
		/>
	</div>
</div>

<style>
	.droppedneedle-topbar {
		min-height: calc(4rem + var(--ms-safe-top));
		padding-top: var(--ms-safe-top);
		padding-block-end: 0;
	}

	@media (min-width: 768px) {
		.droppedneedle-topbar {
			min-height: 4rem;
			padding-top: 0;
		}
	}

	/* The moon and the sun share one cell; the one away turns and fades out. */
	.theme-glyph {
		display: grid;
	}

	.theme-glyph__face {
		display: grid;
		grid-area: 1 / 1;
		transition:
			opacity 300ms ease,
			transform 300ms ease;
	}

	.theme-glyph__face.is-away {
		opacity: 0;
		transform: rotate(90deg) scale(0.6);
	}

	.theme-glyph__face--sun.is-away {
		transform: rotate(-90deg) scale(0.6);
	}
</style>
