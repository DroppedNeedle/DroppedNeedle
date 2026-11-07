<!-- Profile > Appearance: the theme cards and the person's own CSS. -->
<script lang="ts">
	import { authStore } from '$lib/stores/authStore.svelte';
	import CustomCssSection from './CustomCssSection.svelte';
	import ThemeChooser from './ThemeChooser.svelte';
</script>

<ThemeChooser />
{#key authStore.user?.id}
	{#if authStore.user}
		<CustomCssSection />
	{/if}
{/key}
