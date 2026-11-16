// Presentation of the adapter's existing configuration; no runtime defaults.
const connection = {
    id: 'connection', title: 'Подключение',
    description: 'Ключ берётся из OpenCode: сначала сохранённый ключ выбранного провайдера, затем его переменные окружения. Подключите провайдера в OpenCode. Здесь поддержаны API-ключи; OAuth не поддержан.',
};
const catalog = {
    id: 'catalog', title: 'Каталог моделей',
    description: 'Источник метаданных OpenCode. Выбор модели и effort находится в чате.',
};
const textField = (key, title, description, section, { required = false, advanced = true } = {}) => ({
    key, title, description, section, value: { type: 'string', multiline: false, secret: false },
    default: null, required, advanced, unit: null,
});

export const configSchema = { fields: [
    textField('provider', 'Маршрут OpenCode',
        'ID провайдера из каталога OpenCode, например opencode-go для Go или opencode для Zen. Используется для каталога, авторизации и запросов.',
        connection, { required: true, advanced: false }),
    textField('database_path', 'База авторизации OpenCode',
        'Необязательный путь к OpenCode v2 DB. Без значения используется opencode/opencode.db в абсолютном XDG_DATA_HOME, иначе ~/.local/share/opencode/opencode.db. Поддержаны ~/, $HOME/ и ${HOME}/. База открывается только для чтения.',
        connection),
    textField('models_file', 'Локальный каталог моделей',
        'Необязательный путь к native ModelsDev metadata файлу. При заданном файле HTTP-обновление каталога не выполняется.',
        catalog),
    textField('models_url', 'Адрес каталога моделей',
        'Необязательный адрес native ModelsDev metadata endpoint. Не меняет адрес API модели; при заданном models_file HTTP-обновление не выполняется.',
        catalog),
] };
