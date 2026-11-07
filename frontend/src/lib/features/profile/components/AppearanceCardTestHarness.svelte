<script lang="ts">
	import { applyAppearance } from '$lib/features/profile/state/appearance.svelte';
	import Toaster from '$lib/ui/Toaster.svelte';
	import AppearanceCard from './AppearanceCard.svelte';

	applyAppearance();
</script>

<AppearanceCard />
<Toaster />
