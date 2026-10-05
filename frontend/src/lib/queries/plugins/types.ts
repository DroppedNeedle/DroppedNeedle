// The plugin list as the settings page reads it; new fields come straight
// from the generated contract.
import type { components } from '$lib/api/v3/openapi';

export const PLUGIN_SECRET_MASK = 'plugin****';

export interface PluginSettingFieldInfo {
	key: string;
	label: string;
	help: string;
	secret: boolean;
}

export interface PluginInfo {
	name: string;
	display_name: string;
	version: string;
	enabled: boolean;
	capabilities: string[];
	active_capabilities: string[];
	description: string;
	author: string;
	homepage: string;
	error: string | null;
	settings_fields: PluginSettingFieldInfo[];
	settings_values: Record<string, string>;
	ui_entry: string;
	ui_pages: string[];
	ui_external_url: string;
	sources: string[];
	targets: string[];
	permissions: string[];
	runtime?: components['schemas']['PluginRuntimeInfo'] | null;
	install?: components['schemas']['PluginInstallInfo'] | null;
}

export interface PluginListResponse {
	plugins: PluginInfo[];
}
