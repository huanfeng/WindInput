package dictfile

import (
	"bufio"
	"fmt"
	"os"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/fileutil"
)

// LoadUserDictFrom 从指定路径加载用户词库
func LoadUserDictFrom(path string) (*UserDictData, error) {
	file, err := os.Open(path)
	if err != nil {
		if os.IsNotExist(err) {
			return &UserDictData{Words: []UserWord{}}, nil
		}
		return nil, fmt.Errorf("failed to open user dict: %w", err)
	}
	defer file.Close()

	var words []UserWord
	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())

		// 跳过空行和注释
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}

		// 格式: code<tab>text<tab>weight<tab>timestamp
		parts := strings.Split(line, "\t")
		if len(parts) < 2 {
			continue
		}

		code := strings.TrimSpace(parts[0])
		text := strings.TrimSpace(parts[1])

		weight := 100
		if len(parts) >= 3 {
			if w, err := strconv.Atoi(strings.TrimSpace(parts[2])); err == nil {
				weight = w
			}
		}

		var createdAt time.Time
		if len(parts) >= 4 {
			if ts, err := strconv.ParseInt(strings.TrimSpace(parts[3]), 10, 64); err == nil {
				createdAt = time.Unix(ts, 0)
			}
		}
		if createdAt.IsZero() {
			createdAt = time.Now()
		}

		words = append(words, UserWord{
			Code:      code,
			Text:      text,
			Weight:    weight,
			CreatedAt: createdAt,
		})
	}

	if err := scanner.Err(); err != nil {
		return nil, fmt.Errorf("failed to scan user dict: %w", err)
	}

	return &UserDictData{Words: words}, nil
}

// SaveUserDictTo 保存用户词库到指定路径
func SaveUserDictTo(data *UserDictData, path string) error {
	if err := config.EnsureConfigDir(); err != nil {
		return fmt.Errorf("failed to ensure config dir: %w", err)
	}

	// 按编码排序
	sortedWords := make([]UserWord, len(data.Words))
	copy(sortedWords, data.Words)
	sort.Slice(sortedWords, func(i, j int) bool {
		if sortedWords[i].Code != sortedWords[j].Code {
			return sortedWords[i].Code < sortedWords[j].Code
		}
		return sortedWords[i].Text < sortedWords[j].Text
	})

	var sb strings.Builder
	sb.WriteString("# Wind Input 用户词库\n")
	sb.WriteString("# 格式: 编码<tab>词语<tab>权重<tab>时间戳\n")
	sb.WriteString("# 请勿手动编辑此文件\n\n")

	for _, w := range sortedWords {
		sb.WriteString(fmt.Sprintf("%s\t%s\t%d\t%d\n",
			w.Code, w.Text, w.Weight, w.CreatedAt.Unix()))
	}

	return fileutil.AtomicWrite(path, []byte(sb.String()), 0644)
}

// AddUserWord 添加用户词条
// 返回 true 表示新增，false 表示更新已有项
func AddUserWord(data *UserDictData, code, text string, weight int) bool {
	code = strings.ToLower(code)

	// 检查是否已存在
	for i, w := range data.Words {
		if w.Code == code && w.Text == text {
			// 更新权重
			data.Words[i].Weight = weight
			return false
		}
	}

	// 添加新词
	data.Words = append(data.Words, UserWord{
		Code:      code,
		Text:      text,
		Weight:    weight,
		CreatedAt: time.Now(),
	})
	return true
}

// RemoveUserWord 删除用户词条
// 返回 true 表示删除成功
func RemoveUserWord(data *UserDictData, code, text string) bool {
	code = strings.ToLower(code)

	for i, w := range data.Words {
		if w.Code == code && w.Text == text {
			data.Words = append(data.Words[:i], data.Words[i+1:]...)
			return true
		}
	}
	return false
}

// UpdateUserWordWeight 更新用户词条权重
func UpdateUserWordWeight(data *UserDictData, code, text string, weight int) bool {
	code = strings.ToLower(code)

	for i, w := range data.Words {
		if w.Code == code && w.Text == text {
			data.Words[i].Weight = weight
			return true
		}
	}
	return false
}

// SearchUserDict 搜索用户词库
// query 可以是编码或词语的部分
func SearchUserDict(data *UserDictData, query string, limit int) []UserWord {
	query = strings.ToLower(query)
	var result []UserWord

	for _, w := range data.Words {
		if strings.Contains(strings.ToLower(w.Code), query) ||
			strings.Contains(w.Text, query) {
			result = append(result, w)
		}
	}

	// 按权重排序
	sort.Slice(result, func(i, j int) bool {
		return result[i].Weight > result[j].Weight
	})

	if limit > 0 && len(result) > limit {
		result = result[:limit]
	}

	return result
}

// GetWordsByCode 获取指定编码的所有词条
func GetWordsByCode(data *UserDictData, code string) []UserWord {
	code = strings.ToLower(code)
	var result []UserWord

	for _, w := range data.Words {
		if w.Code == code {
			result = append(result, w)
		}
	}

	// 按权重排序
	sort.Slice(result, func(i, j int) bool {
		return result[i].Weight > result[j].Weight
	})

	return result
}

// GetWordCount 获取词条总数
func GetWordCount(data *UserDictData) int {
	return len(data.Words)
}

// ImportUserDict 从文件导入词条（合并）
// 支持格式：编码<tab>词语<tab>权重
func ImportUserDict(data *UserDictData, path string) (int, error) {
	importData, err := LoadUserDictFrom(path)
	if err != nil {
		return 0, err
	}

	addedCount := 0
	for _, w := range importData.Words {
		if AddUserWord(data, w.Code, w.Text, w.Weight) {
			addedCount++
		}
	}

	return addedCount, nil
}

// ExportUserDict 导出词条到文件
func ExportUserDict(data *UserDictData, path string) error {
	return SaveUserDictTo(data, path)
}
