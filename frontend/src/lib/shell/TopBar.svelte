<!-- The bar along the top: logo, search, service health and the profile link. -->
<script lang="ts">
	import { goto } from '$app/navigation';
	import { albumHref, artistHref } from '$lib/utils/entityRoutes';
	import { UserRound } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Logo from '$lib/ui/Logo.svelte';
	import SearchSuggestions from '$lib/features/search/components/SearchSuggestions.svelte';
	import ServiceHealthIndicator from '$lib/shell/ServiceHealthIndicator.svelte';
	import { getApiUrl } from '$lib/api/api-utils';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import type { SuggestResult } from '$lib/features/search/queries/types';

	let query = $state('');

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

<div class="droppedneedle-topbar navbar sticky top-0 z-50 border-b border-base-300 bg-base-100">
	<div class="navbar-start w-auto">
		<Button
			variant="ghost"
			href={withBasePath('/')}
			class="px-2 max-xs:hidden sm:px-3"
			aria-label="Home"
		>
			<Logo label="" class="hidden h-7 text-base-content sm:block" />
			<Logo variant="icon" label="" class="block h-7 text-base-content sm:hidden" />
		</Button>
	</div>
	<div class="navbar-center min-w-0 shrink grow basis-0 justify-center px-1 sm:px-4">
		<div class="w-full max-w-2xl">
			<SearchSuggestions
				bind:query
				onSearch={handleSearch}
				onSelect={handleSelect}
				id="navbar-suggest"
			/>
		</div>
	</div>
	<div class="navbar-end w-auto pr-1 sm:pr-2">
		<ServiceHealthIndicator />
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
</style>
