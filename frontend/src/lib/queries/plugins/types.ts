// Hand-mirrors backend/api/v1/schemas/plugins.py (snake_case wire format).

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
}

export interface PluginListResponse {
	plugins: PluginInfo[];
}

export interface SourcePluginInfo {
	name: string;
	display_name: string;
	description: string;
}

export interface SourceListResponse {
	sources: SourcePluginInfo[];
}

export interface SourceItem {
	id: string;
	title: string;
	artist: string;
	detail: string;
}

export interface SourceSearchResponse {
	items: SourceItem[];
}
