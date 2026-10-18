<!-- The bar along the top: logo, search, service health and the profile link. -->
<script lang="ts">
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { UserRound } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import SearchSuggestions from '$lib/components/SearchSuggestions.svelte';
	import ServiceHealthIndicator from '$lib/components/ServiceHealthIndicator.svelte';
	import { getApiUrl } from '$lib/api/api-utils';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import type { SuggestResult } from '$lib/types';

	let query = $state('');

	function handleSearch() {
		if (query.trim()) {
			goto(withBasePath(`/search?q=${encodeURIComponent(query)}`));
		}
	}

	function handleSelect(result: SuggestResult) {
		const routeId = result.type === 'artist' ? '/artist/[id]' : '/album/[id]';
		goto(resolve(routeId, { id: result.musicbrainz_id }));
	}
</script>

<div class="droppedneedle-topbar navbar bg-base-100/95 backdrop-blur shadow-sm sticky top-0 z-50">
	<div class="navbar-start w-auto">
		<Button
			variant="ghost"
			href={withBasePath('/')}
			class="px-2 max-xs:hidden sm:px-4"
			aria-label="Home"
		>
			<img
				src={withBasePath('/logo_wide.png')}
				alt="DroppedNeedle"
				class="dn-logo h-8 hidden sm:block"
			/>
			<img
				src={withBasePath('/logo_icon.png')}
				alt="DroppedNeedle"
				class="dn-logo h-8 block sm:hidden"
			/>
		</Button>
	</div>
	<div class="navbar-center min-w-0 grow justify-center px-1 sm:px-4">
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
	}

	@media (min-width: 768px) {
		.droppedneedle-topbar {
			min-height: 4rem;
			padding-top: 0;
		}
	}
</style>
